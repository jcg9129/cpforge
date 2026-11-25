use super::{model::*, native::*, run::*, value::*};
use crate::config::{Combine, Comparator, RelativeBase};
use crate::jobs::engine as e;
use crate::report as r;
use std::path::Path;

/// A failed cwd step cannot also claim a successfully cleared overflow.
pub(super) fn cwd_notes(notes: &[r::RunNote], bytes: Option<u64>) -> Result<()> {
    let unmeasured = notes
        .iter()
        .filter(|note| {
            matches!(
                note,
                r::RunNote::CwdUnmeasuredTimeout
                    | r::RunNote::CwdUnmeasuredIo
                    | r::RunNote::CwdUnmeasuredCancelled
            )
        })
        .count();
    if unmeasured > 1
        || (unmeasured != 0 && (bytes.is_some() || notes.contains(&r::RunNote::CwdOverflow)))
    {
        return Err(Error("inconsistent cwd notes"));
    }
    Ok(())
}

pub(super) fn relative(path: &Path, base: Option<&Path>, max: usize) -> Result<Value> {
    let path = if path.is_absolute() {
        path.strip_prefix(base.ok_or(Error("absolute path needs root"))?)
            .map_err(|_| Error("path outside root"))?
    } else {
        path
    };
    rel(path, max)
}
pub(super) fn sequence(epoch: &crate::storage::job::Epoch, value: u64) -> Value {
    obj([("epoch", s(epoch.as_str())), ("n", u(value))])
}
pub(super) fn job(v: &e::JobIdentity, workspace: Option<&Path>) -> Result<Value> {
    Ok(obj([
        ("id", s(v.id.as_str())),
        ("dir", relative(&v.dir, workspace, 50)?),
        ("sequence", sequence(&v.sequence.0, v.sequence.1)),
        ("kind", s(v.kind.wire_token())),
    ]))
}
pub(super) fn problem(v: &Problem, workspace: Option<&Path>) -> Result<Value> {
    let id = v
        .identity
        .strip_prefix("local:")
        .ok_or(Error("invalid problem identity"))?;
    ident(id)?;
    Ok(obj([
        ("identity", s(&v.identity)),
        ("name", ident(&v.name)?),
        ("kind", text(&v.kind)),
        ("dir", relative(&v.dir, workspace, 75)?),
    ]))
}
pub(super) fn float(v: f64) -> Value {
    obj([
        (
            "text",
            s(if v.is_nan() {
                "nan".into()
            } else {
                format!("{v:e}")
            }),
        ),
        ("bits", s(format!("{:016x}", v.to_bits()))),
    ])
}
pub(super) fn comparator(v: &Comparator) -> Result<Value> {
    Ok(match v {
        Comparator::Exact => kind("exact"),
        Comparator::Tokens => kind("tokens"),
        Comparator::Lines => kind("lines"),
        Comparator::Float(v) => obj([
            ("kind", s("float")),
            ("abs", opt(v.abs, float)),
            ("rel", opt(v.rel, float)),
            (
                "rel_base",
                if v.rel.is_some() {
                    s(match v.rel_base {
                        RelativeBase::Answer => "answer",
                        RelativeBase::Max => "max",
                        RelativeBase::Max1 => "max1",
                    })
                } else {
                    Value::Null
                },
            ),
            (
                "combine",
                if v.abs.is_some() && v.rel.is_some() {
                    s(match v.combine {
                        Combine::Either => "either",
                        Combine::Both => "both",
                    })
                } else {
                    Value::Null
                },
            ),
            (
                "ulps",
                match v.ulps {
                    Some(v) => s(i64::try_from(v)
                        .map_err(|_| Error("ulps exceeds i64"))?
                        .to_string()),
                    None => Value::Null,
                },
            ),
            ("int_exact", b(v.int_exact)),
        ]),
    })
}
pub(super) fn target(v: &Target) -> Result<Value> {
    let default_check = v.compare.as_ref().map(|_| r::CheckConfig::default());
    let value = obj([
        (
            "name",
            v.name
                .as_deref()
                .map(ident)
                .transpose()?
                .unwrap_or(Value::Null),
        ),
        (
            "language",
            v.language
                .as_deref()
                .map(ident)
                .transpose()?
                .unwrap_or(Value::Null),
        ),
        (
            "profile",
            v.profile
                .as_deref()
                .map(ident)
                .transpose()?
                .unwrap_or(Value::Null),
        ),
        ("build_free", b(v.build_free)),
        (
            "compare",
            v.compare
                .as_ref()
                .map(comparator)
                .transpose()?
                .unwrap_or(Value::Null),
        ),
        (
            "check",
            v.check
                .as_ref()
                .or(default_check.as_ref())
                .map(super::judging::config)
                .transpose()?
                .unwrap_or(Value::Null),
        ),
        (
            "io",
            v.io.as_ref()
                .map(super::io::config)
                .transpose()?
                .unwrap_or(Value::Null),
        ),
        (
            "run_deadline",
            match v.run_deadline {
                Some((us, source)) => {
                    if !matches!(source, "configured" | "derived" | "default" | "cli") {
                        return Err(Error("invalid deadline source"));
                    }
                    obj([("us", u(us)), ("source", s(source))])
                }
                None => Value::Null,
            },
        ),
        (
            "published_limits",
            opt(v.published_limits, |(time, memory)| {
                obj([("time_ms", opt(time, n)), ("memory_bytes", opt(memory, u))])
            }),
        ),
        (
            "local_limits",
            checked_array(&v.local_limits, 2, |v| limit(v, false))?,
        ),
    ]);
    Ok(value)
}
pub(super) fn capture_check(v: &crate::jobs::CaptureCheck) -> Result<Value> {
    Ok(match v {
        crate::jobs::CaptureCheck::Checked => kind("checked"),
        crate::jobs::CaptureCheck::Unchecked { reasons } => obj([
            ("kind", s("unchecked")),
            (
                "reasons",
                checked_array(reasons, 4, |v| {
                    Ok(s(match v {
                        crate::jobs::CaptureCheckReason::ShellStage => "shell_stage",
                    }))
                })?,
            ),
        ]),
    })
}
pub(super) fn capture(v: &e::CaptureReport) -> Result<Value> {
    Ok(obj([
        ("check", capture_check(&v.check)?),
        (
            "sources",
            array(v.sources.iter().take(16), |v| {
                nsum(&native_bytes(v.as_os_str()), 128)
            }),
        ),
        (
            "sources_dropped",
            dropped(v.sources.len(), 16, v.sources_dropped),
        ),
        (
            "support_files",
            array(v.support_files.iter().take(16), |v| {
                nsum(&native_bytes(v.as_os_str()), 128)
            }),
        ),
        (
            "support_files_dropped",
            dropped(v.support_files.len(), 16, v.support_files_dropped),
        ),
        (
            "counts",
            obj([
                ("sources", n(v.counts[0])),
                ("support_files", n(v.counts[1])),
            ]),
        ),
        ("bytes", u(v.bytes)),
        ("lists_sha256", hex(&v.lists_sha256)),
        (
            "manifest",
            rel(
                v.manifest
                    .as_deref()
                    .ok_or(Error("ready capture needs published manifest"))?,
                26,
            )?,
        ),
    ]))
}
pub(super) fn build_result(v: &r::BuildResult) -> Result<Value> {
    Ok(match v {
        r::BuildResult::Succeeded => kind("succeeded"),
        r::BuildResult::Cancelled => kind("cancelled"),
        r::BuildResult::InternalError { invariant } => obj([
            ("kind", s("internal_error")),
            ("invariant", s(invariant.wire_token())),
        ]),
        r::BuildResult::Infrastructure { causes: c } => obj([
            ("kind", s("infrastructure")),
            ("causes", causes(c)),
            ("causes_dropped", n(c.causes_dropped)),
        ]),
        r::BuildResult::Failed { cause: c } => obj([
            ("kind", s("failed")),
            (
                "cause",
                match c {
                    r::BuildCause::Run(v) => class(v)?,
                    r::BuildCause::MissingOutput {
                        names,
                        names_dropped,
                    } => obj([
                        ("kind", s("missing_output")),
                        (
                            "names",
                            array(names.iter().take(16), |v| nsum(&native_bytes(v), 128)),
                        ),
                        ("names_dropped", dropped(names.len(), 16, *names_dropped)),
                    ]),
                    r::BuildCause::GeneratedProgramInvalid { name, reason } => obj([
                        ("kind", s("generated_program_invalid")),
                        ("name", nsum(&native_bytes(name), 128)),
                        ("reason", text(reason)),
                    ]),
                    r::BuildCause::OutputTooLarge { bytes, cap } => obj([
                        ("kind", s("output_too_large")),
                        ("bytes", u(*bytes)),
                        ("cap", u(*cap)),
                    ]),
                },
            ),
        ]),
    })
}
pub(super) fn build(v: &e::BuildReport, root: &Path) -> Result<Value> {
    if !matches!(v.result, r::BuildResult::Succeeded) && !v.artifacts.is_empty() {
        return Err(Error("only successful builds have validated artifacts"));
    }
    Ok(obj([
        ("execution", n(v.execution)),
        ("build_id", opt(v.build_id.as_ref(), |v| s(v.as_str()))),
        (
            "output_root",
            v.output_root
                .as_deref()
                .map(|v| relative(v, Some(root), 15))
                .transpose()?
                .unwrap_or(Value::Null),
        ),
        ("result", build_result(&v.result)?),
        (
            "artifacts",
            array(v.artifacts.iter().take(16), |v| nsum(&native_bytes(v), 128)),
        ),
        ("artifacts_dropped", dropped(v.artifacts.len(), 16, 0)),
    ]))
}
pub(super) fn check(v: &r::CheckKind) -> Result<Value> {
    if let r::CheckKind::Judged { outcome } = v {
        if let r::JudgedCheck::Scored { raw, scale, .. } = outcome.as_ref() {
            if raw > scale {
                return Err(Error("check score exceeds scale"));
            }
        }
        return super::judging::serialized(outcome);
    }
    let mut fields = vec![("kind", s(v.wire_token()))];
    if let Some(fail) = v.fail_token() {
        fields.push(("fail", s(fail)));
    }
    if let Some(class) = v.class_token() {
        fields.push(("class", s(class)));
    }
    if let Some(reason) = v.reason_token() {
        fields.push(("reason", s(reason)));
    }
    match v {
        r::CheckKind::Failed { diag } | r::CheckKind::CheckerFailed { diag } => {
            let bytes =
                serde_json::to_vec(diag).map_err(|_| Error("diagnostic encoding failed"))?;
            if bytes.len() > 2048 {
                return Err(Error("comparison diagnostic exceeds bound"));
            }
            fields.push(("diag", Value::Verbatim(bytes)));
        }
        r::CheckKind::CheckError { os_error } => fields.push(("os_error", opt(*os_error, i))),
        r::CheckKind::InternalError { invariant } => {
            fields.push(("invariant", s(invariant.wire_token())))
        }
        _ => {}
    }
    Ok(Value::Object(fields))
}
pub(super) fn verdict(v: &r::TestVerdict) -> Value {
    let mut fields = vec![("kind", s(v.wire_token()))];
    match v {
        r::TestVerdict::NotRun { reason } => fields.push(("reason", s(reason.wire_token()))),
        r::TestVerdict::InternalError { invariant } => {
            fields.push(("invariant", s(invariant.wire_token())))
        }
        _ => {}
    }
    Value::Object(fields)
}
pub(super) fn test(v: &e::TestReport) -> Result<Value> {
    use crate::tests_store::{FrozenAnswer, Origin};
    super::io::test(v)?;
    let (answer, digest) = match &v.frozen.answer {
        FrozenAnswer::Absent => ("absent", Value::Null),
        FrozenAnswer::Present { len, sha256, .. } => {
            (if *len == 0 { "empty" } else { "present" }, hex(sha256))
        }
    };
    let value = obj([
        ("id", ident(v.frozen.id.as_str())?),
        (
            "origin",
            s(match v.frozen.origin {
                Origin::Sample => "sample",
                Origin::Custom => "custom",
                Origin::Stress => "stress",
                Origin::Imported => "imported",
            }),
        ),
        ("answer", s(answer)),
        ("input_sha256", hex(&v.frozen.input.sha256)),
        ("answer_sha256", digest),
        (
            "executions",
            obj([
                ("first", n(v.first_execution)),
                ("count", n(v.execution_count)),
            ]),
        ),
        (
            "class",
            v.class
                .as_ref()
                .map(class)
                .transpose()?
                .unwrap_or(Value::Null),
        ),
        (
            "check",
            v.check
                .as_ref()
                .map(|v| check(&v.kind))
                .transpose()?
                .unwrap_or(Value::Null),
        ),
        (
            "diagnostic",
            b(v.check.as_ref().is_some_and(|v| v.diagnostic)),
        ),
        ("verdict", opt(v.verdict.as_ref(), verdict)),
        ("score", super::judging::serialized(&v.score)?),
        (
            "validation",
            v.validation
                .as_ref()
                .map(super::judging::validation)
                .transpose()?
                .unwrap_or(Value::Null),
        ),
        (
            "judges",
            checked_array(&v.judges, 2, super::judging::judge)?,
        ),
        ("session", Value::Null),
        (
            "output_file",
            v.output_file
                .as_ref()
                .map(super::io::output_file)
                .transpose()?
                .unwrap_or(Value::Null),
        ),
    ]);
    let shape = super::ReportShape {
        validator: v.validation.is_some(),
        judges: v.judges.len() as u8,
        interactive: false,
        output_file: v.output_file.is_some(),
    };
    bounded(&value, shape.test_bound()?)?;
    Ok(value)
}
pub(super) fn aggregate(v: &e::Aggregate) -> Result<Value> {
    let value = obj([
        (
            "test",
            v.test
                .as_deref()
                .map(ident)
                .transpose()?
                .unwrap_or(Value::Null),
        ),
        ("iterations", n(v.iterations)),
        ("succeeded", n(v.succeeded)),
        (
            "wall_us",
            match v.wall {
                Some([min, median, max]) => obj([
                    ("min", dur(min)?),
                    ("median", dur(median)?),
                    ("max", dur(max)?),
                ]),
                None => Value::Null,
            },
        ),
    ]);
    bounded(&value, 256)?;
    Ok(value)
}
macro_rules! counts_object {($v:expr; $($field:ident),+)=>{obj([$( (stringify!($field),n($v.$field)) ),+])};}
fn cov(v: r::Cov) -> Value {
    counts_object!(v;planned,started,not_started,transferred,skipped,admitted,invalid)
}
pub(super) fn counts(v: r::Counts) -> Value {
    obj([
        ("primary", cov(v.primary)),
        ("hooks", cov(v.hooks)),
        ("tests", n(v.tests)),
        (
            "by_verdict",
            counts_object!(v.by_verdict;passed,wrong_answer,unchecked,checker_failed,wall_deadline,output_budget,signal,nonzero_exit,cancelled,infrastructure,not_run,internal_error,presentation_error,scored,session_limit,idle),
        ),
        (
            "by_class",
            counts_object!(v.by_class;exited_zero,nonzero_exit,signal,wall_deadline,output_budget,cancelled,infrastructure,skipped,unknown),
        ),
    ])
}
pub(super) fn phase(v: e::CancelPhase) -> &'static str {
    match v {
        e::CancelPhase::Primary => "primary",
        e::CancelPhase::Fact => "fact",
        e::CancelPhase::Hooks => "hooks",
        e::CancelPhase::Settlement => "settlement",
        e::CancelPhase::Closure => "closure",
    }
}
pub(super) fn cancellation(v: e::Cancellation) -> Result<Value> {
    Ok(obj([
        ("requested_at_us", dur(v.requested_at)?),
        (
            "source",
            s(match v.source {
                e::CancelSource::Console => "console",
                e::CancelSource::Terminate => "terminate",
                e::CancelSource::Api => "api",
            }),
        ),
        ("phase", s(phase(v.phase))),
    ]))
}
pub(super) fn pre_exit(v: PreDeliveryExit) -> Result<Value> {
    if v.reraise
        .is_some_and(|v| !matches!(v, "sigint" | "sigterm"))
    {
        return Err(Error("invalid reraise signal"));
    }
    Ok(obj([
        ("code", code(v.code)?),
        ("reraise", opt(v.reraise, s)),
    ]))
}
pub(super) fn result(v: &e::ReportResult, meta: &e::ReportDetails) -> Result<Value> {
    Ok(obj([
        ("verdict", s(v.verdict.wire_token())),
        ("primary_verdict", s(v.primary_verdict.wire_token())),
        ("partial", b(v.partial)),
        (
            "cancellation",
            v.cancellation
                .map(cancellation)
                .transpose()?
                .unwrap_or(Value::Null),
        ),
        ("pre_delivery_exit", pre_exit(meta.pre_delivery_exit)?),
        ("counts", counts(v.counts)),
        (
            "internal",
            v.internal
                .as_ref()
                .map(|v| internal(v, &meta.internal_message))
                .transpose()?
                .unwrap_or(Value::Null),
        ),
        (
            "score",
            meta.score
                .as_ref()
                .map(super::judging::aggregate)
                .transpose()?
                .unwrap_or(Value::Null),
        ),
    ]))
}
fn internal(v: &r::InternalFailure, message: &str) -> Result<Value> {
    let mut locus = vec![("kind", s(v.locus.wire_token()))];
    match &v.locus {
        r::InternalLocus::Execution { index } => locus.push(("index", n(*index))),
        r::InternalLocus::Test { test } => locus.push(("test", ident(test)?)),
        r::InternalLocus::Phase { phase } => locus.push(("phase", token(phase)?)),
        _ => {}
    }
    Ok(obj([
        ("invariant", s(v.invariant.wire_token())),
        ("locus", Value::Object(locus)),
        ("message", text(message)),
    ]))
}
pub(super) fn fact_outcome(v: &FactOutcome) -> Result<Value> {
    Ok(match v {
        FactOutcome::Published => kind("published"),
        FactOutcome::Superseded => kind("superseded"),
        FactOutcome::NotAttempted { reason } => {
            if !matches!(*reason, "cancelled" | "not_completed") {
                return Err(Error("invalid fact reason"));
            }
            obj([("kind", s("not_attempted")), ("reason", s(*reason))])
        }
        FactOutcome::NotPublished { reason, message } => {
            if !matches!(
                *reason,
                "sequence_unavailable"
                    | "epoch_changed"
                    | "fact_too_new"
                    | "fact_unreadable"
                    | "lock_busy"
                    | "io"
            ) {
                return Err(Error("invalid fact reason"));
            }
            obj([
                ("kind", s("not_published")),
                ("reason", s(*reason)),
                ("message", opt(message.as_deref(), text)),
            ])
        }
    })
}
pub(super) fn fact(v: &Fact) -> Result<Value> {
    Ok(obj([
        ("outcome", fact_outcome(&v.outcome)?),
        (
            "warnings",
            checked_array(&v.warnings, 2, |v| {
                if !matches!(*v, "durability_uncertain" | "cleanup_pending") {
                    return Err(Error("invalid fact warning"));
                }
                Ok(s(*v))
            })?,
        ),
    ]))
}
pub(super) fn hook_outcome(v: &HookResult) -> Result<Value> {
    Ok(match v {
        HookResult::NotPlanned { reason } => {
            if !matches!(
                *reason,
                "untrusted" | "disabled" | "config_override" | "depth"
            ) {
                return Err(Error("invalid hook reason"));
            }
            obj([("kind", s("not_planned")), ("reason", s(*reason))])
        }
        HookResult::Planned(v) => {
            let mut fields = vec![("kind", s(v.wire_token()))];
            match v {
                r::HookOutcome::Failed { class } => fields.push(("class", s(class.wire_token()))),
                r::HookOutcome::Skipped { reason } => {
                    fields.push(("reason", s(reason.wire_token())))
                }
                r::HookOutcome::InternalError { invariant } => {
                    fields.push(("invariant", s(invariant.wire_token())))
                }
                _ => {}
            }
            Value::Object(fields)
        }
    })
}
pub(super) fn hook(v: &Hook) -> Result<Value> {
    if !matches!(v.trigger, "on_switch" | "after_test") {
        return Err(Error("invalid hook trigger"));
    }
    Ok(obj([
        ("name", ident(&v.name)?),
        ("trigger", token(v.trigger)?),
        ("execution", opt(v.execution, n)),
        ("outcome", hook_outcome(&v.outcome)?),
    ]))
}
pub(super) fn warning(v: &Warning) -> Result<Value> {
    match &v.kind {
        WarningKind::Prune { detail, .. }
            if !matches!(
                *detail,
                "rename_failed"
                    | "newer_schema"
                    | "lease_unreadable"
                    | "unmeasurable"
                    | "measurement_incomplete"
                    | "reclaim_leftover"
                    | "reclaim_name_taken"
            ) =>
        {
            return Err(Error("invalid prune warning detail"));
        }
        WarningKind::Persist { artefact, .. }
            if !matches!(*artefact, "spec" | "record" | "cleanup_log") =>
        {
            return Err(Error("invalid persistence artefact"));
        }
        WarningKind::Durability { detail, .. }
            if !matches!(*detail, "uncertain" | "cleanup_pending") =>
        {
            return Err(Error("invalid durability detail"));
        }
        WarningKind::Retention { detail, .. }
            if !matches!(*detail, "cwd_overflow" | "workspace_over_target") =>
        {
            return Err(Error("invalid retention detail"));
        }
        // Only judge roles have a profile, so only they have header evidence.
        WarningKind::TestlibHeader { role, .. }
            if matches!(role, r::RoleName::Generator | r::RoleName::Brute) =>
        {
            return Err(Error("testlib_header warning needs a judge role"));
        }
        _ => {}
    }
    let (kind, mut fields) = match &v.kind {
        WarningKind::JudgeScratch { run, detail } => (
            "judge_scratch",
            vec![
                ("run", run_id(run)?),
                ("detail", super::judging::serialized(detail)?),
            ],
        ),
        WarningKind::TestlibHeader { role, detail } => (
            "testlib_header",
            vec![
                ("role", s(role.wire_token())),
                ("detail", super::judging::serialized(detail)?),
            ],
        ),
        WarningKind::IoScratch { run, detail } => (
            "io_scratch",
            vec![("run", run_id(run)?), ("detail", s(detail.wire_token()))],
        ),
        WarningKind::Prune { job, detail } => (
            "prune",
            vec![
                ("job", opt(job.as_ref(), |v| s(v.as_str()))),
                ("detail", token(detail)?),
            ],
        ),
        WarningKind::Recovery { paths, first } => (
            "recovery",
            vec![
                ("paths", n(*paths)),
                (
                    "first",
                    opt(first.as_ref(), |v| nsum(&native_bytes(v.as_os_str()), 128)),
                ),
            ],
        ),
        WarningKind::Persist {
            artefact,
            run,
            error,
        } => (
            "persist",
            vec![
                ("artefact", token(artefact)?),
                ("run", run_id(run)?),
                ("error", persist(error, &v.message)),
            ],
        ),
        WarningKind::Durability { artefact, detail } => (
            "durability",
            vec![("artefact", token(artefact)?), ("detail", token(detail)?)],
        ),
        WarningKind::Retention { detail, run, bytes } => (
            "retention",
            vec![
                ("detail", token(detail)?),
                (
                    "run",
                    run.as_deref()
                        .map(run_id)
                        .transpose()?
                        .unwrap_or(Value::Null),
                ),
                ("bytes", u(*bytes)),
            ],
        ),
        WarningKind::CleanupUnverified { runs } => ("cleanup_unverified", vec![("runs", n(*runs))]),
        WarningKind::Fact(v) => ("fact", vec![("outcome", fact_outcome(v)?)]),
        WarningKind::Hook { name, outcome } => (
            "hook",
            vec![("name", ident(name)?), ("outcome", hook_outcome(outcome)?)],
        ),
        WarningKind::HooksDisabled { reason } => {
            ("hooks_disabled", vec![("reason", token(reason)?)])
        }
        WarningKind::LateCancellation { phase: v } => {
            ("late_cancellation", vec![("phase", s(phase(*v)))])
        }
        WarningKind::ProgressDropped { lines } => ("progress_dropped", vec![("lines", n(*lines))]),
    };
    fields.insert(0, ("kind", s(kind)));
    fields.push(("message", text(&v.message)));
    let value = Value::Object(fields);
    bounded(&value, 768)?;
    Ok(value)
}
pub(super) fn run_id(v: &str) -> Result<Value> {
    if v.len() != 6 || !v.starts_with('r') || !v.as_bytes()[1..].iter().all(u8::is_ascii_digit) {
        return Err(Error("invalid run id"));
    }
    Ok(s(v))
}
pub(crate) fn engine_warning(
    v: &e::Warning,
    cancellation: Option<e::Cancellation>,
) -> Result<Warning> {
    Ok(Warning {
        message: match v {
            e::Warning::LateCancellation => "cancellation requested after primary completion",
            e::Warning::Prune { .. } => "job protected during pruning",
            e::Warning::Recovery { .. } => "interrupted publications recovered",
            e::Warning::Durability { .. } => "publication durability uncertain",
            e::Warning::CleanupUnverified { .. } => "cleanup not verified",
            e::Warning::Persistence { .. } => "artefact publication failed",
        }
        .into(),
        kind: match v {
            e::Warning::LateCancellation => WarningKind::LateCancellation {
                phase: cancellation
                    .ok_or(Error("late cancellation warning without intent"))?
                    .phase,
            },
            e::Warning::Prune { job, detail } => WarningKind::Prune {
                job: job.clone(),
                detail,
            },
            e::Warning::Recovery { paths, first } => WarningKind::Recovery {
                paths: *paths,
                first: first.clone(),
            },
            e::Warning::Durability { artefact, detail } => {
                WarningKind::Durability { artefact, detail }
            }
            e::Warning::CleanupUnverified { runs } => {
                WarningKind::CleanupUnverified { runs: *runs }
            }
            e::Warning::Persistence {
                artefact,
                run,
                error,
            } => WarningKind::Persist {
                artefact,
                run: run.as_str().to_owned(),
                error: error.clone(),
            },
        },
    })
}
pub(super) fn timing(v: &e::Timing) -> Result<Value> {
    Ok(obj([
        ("started_at", s(timestamp(v.started_at)?)),
        ("finished_at", s(timestamp(v.finished_at)?)),
        ("duration_us", dur(v.duration)?),
    ]))
}
/// Gregorian UTC with millisecond floor, including times before the Unix epoch.
pub fn timestamp(v: std::time::SystemTime) -> Result<String> {
    let ns = match v.duration_since(std::time::UNIX_EPOCH) {
        Ok(v) => v.as_nanos() as i128,
        Err(v) => -(v.duration().as_nanos() as i128),
    };
    let ms = ns.div_euclid(1_000_000);
    let mut days = ms.div_euclid(86_400_000) + 719_528;
    if !(0..3_652_425).contains(&days) {
        return Err(Error("timestamp year out of range"));
    }
    let mut year = days / 146_097 * 400;
    days %= 146_097;
    let leap = |y: i128| y % 4 == 0 && (y % 100 != 0 || y % 400 == 0);
    loop {
        let n = if leap(year) { 366 } else { 365 };
        if days < n {
            break;
        }
        days -= n;
        year += 1;
    }
    let months = [
        31,
        if leap(year) { 29 } else { 28 },
        31,
        30,
        31,
        30,
        31,
        31,
        30,
        31,
        30,
        31,
    ];
    let mut month = 0;
    while days >= months[month] {
        days -= months[month];
        month += 1;
    }
    let clock = ms.rem_euclid(86_400_000);
    Ok(format!(
        "{year:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:03}Z",
        month + 1,
        days + 1,
        clock / 3_600_000,
        clock / 60_000 % 60,
        clock / 1000 % 60,
        clock % 1000
    ))
}
pub(super) fn diagnostic(v: &Diagnostic) -> Value {
    obj([
        (
            "file",
            opt(v.file.as_ref(), |v| nsum(&native_bytes(v.as_os_str()), 256)),
        ),
        ("key", opt(v.key.as_deref(), text)),
        ("line", opt(v.line, n)),
        ("col", opt(v.col, n)),
        ("message", text(&v.message)),
    ])
}
pub(super) fn document_error(v: &DocumentError) -> Result<Value> {
    Ok(obj([
        ("kind", token(v.kind)?),
        ("message", text(&v.message)),
        (
            "path",
            opt(v.path.as_ref(), |v| nsum(&native_bytes(v.as_os_str()), 256)),
        ),
    ]))
}
pub(super) fn persistence(v: &ReportPersistence) -> Result<Value> {
    let (report, durability) = match v {
        ReportPersistence::Published {
            path, uncertain, ..
        } => (
            obj([("kind", s("published")), ("path", rel(path, 60)?)]),
            s(if *uncertain { "uncertain" } else { "ok" }),
        ),
        ReportPersistence::Failed {
            error:
                crate::jobs::engine::PersistError::QueueFull
                | crate::jobs::engine::PersistError::Cancelled,
            ..
        } => {
            return Err(Error("report cannot fail for queue credits"));
        }
        ReportPersistence::Failed { error, message } => (
            obj([("kind", s("failed")), ("error", persist(error, message))]),
            Value::Null,
        ),
        ReportPersistence::Refusal => (
            obj([("kind", s("not_attempted")), ("reason", s("refusal"))]),
            Value::Null,
        ),
    };
    Ok(obj([("report", report), ("durability", durability)]))
}
