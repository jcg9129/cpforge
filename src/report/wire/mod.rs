//! Canonical schema-1 JSON. These pure encoders return compact UTF-8 bytes,
//! without a trailing newline. Transport and publication belong to callers.
//!
//! The engine owns all finalized evidence; encoding requires no supplemental metadata.
//!
//! E §2 and §6 define spec/record contents and limits, but do not prescribe
//! JSON document names or nested object spellings. `execution_spec` and
//! `run_record` follow declaration order, with report vocabulary for shared
//! variants. Config tables live under `config`, preserving effective key order.
//! These encodings do not compute or change the independent binary spec digest.
//! The engine also supplies the already-selected pre-delivery status;
//! encoding has no process-status, publication, delivery, or settlement effects.
mod artifacts;
mod events;
mod io;
mod judging;
mod model;
mod native;
pub mod read;
mod roles;
mod run;
mod schema;
mod status;
mod value;
pub(crate) use status::{ProblemStatus, encode_status};

pub use artifacts::*;
pub use events::*;
pub use model::*;
pub use native::{
    base64, decode_base64, decode_native, encode_bytes, encode_native, encode_native_summary,
    native_bytes,
};
pub use run::{encode_run_summary, ntstatus_name};
pub(crate) use schema::engine_warning;
pub use schema::timestamp;
pub use value::{Error, Result};

use crate::jobs::engine as e;
use schema::*;
use value::*;

pub const REPORT_HEADER_BOUND: usize = 128 * 1024;
pub const EXECUTION_ALLOWANCE: usize = 9 * 1024;
pub const REFUSAL_BOUND: usize = 104 * 1024;
pub const EVENT_BOUND: usize = 7 * 1024;

pub fn report_bound(planned: usize, tests: usize, aggregates: usize) -> Result<usize> {
    shaped_report_bound(planned, tests, aggregates, ReportShape::default())
}

/// The per-test shape of §3.7.2, fixed by the plan, including skipped tests.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ReportShape {
    pub validator: bool,
    pub judges: u8,
    pub interactive: bool,
    pub output_file: bool,
}
impl ReportShape {
    pub fn test_bound(self) -> Result<usize> {
        if self.judges > 2 || (self.interactive && self.output_file) {
            return Err(Error("invalid report shape"));
        }
        Ok(3 * 1024
            + usize::from(self.validator) * 2560
            + usize::from(self.judges) * 11 * 1024
            + usize::from(self.interactive) * 19 * 1024
            + usize::from(self.output_file) * 192)
    }
}
pub fn shaped_report_bound(
    planned: usize,
    tests: usize,
    aggregates: usize,
    shape: ReportShape,
) -> Result<usize> {
    if planned > 10_000
        || tests > planned
        || aggregates > planned
        || ((shape.validator || shape.judges != 0 || shape.interactive) && tests > 1000)
    {
        return Err(Error("report population exceeds plan cap"));
    }
    Ok(REPORT_HEADER_BOUND
        + EXECUTION_ALLOWANCE * planned
        + shape.test_bound()? * tests
        + 256 * aggregates)
}
/// Exactly the report plus replacement temporary, before the first admission.
/// None is the comparison-only shape, with the same population terms.
pub fn report_reservation(
    planned: usize,
    tests: usize,
    aggregates: usize,
    shape: Option<ReportShape>,
) -> Result<u64> {
    let bound = match shape {
        Some(shape) => shaped_report_bound(planned, tests, aggregates, shape)?,
        None => report_bound(planned, tests, aggregates)?,
    };
    let bytes = 2 * bound as u64;
    if bytes > 256 * 1024 * 1024 {
        return Err(Error("report reservation exceeds plan cap"));
    }
    Ok(bytes)
}
pub(crate) fn report_data_bound(v: &e::ReportData) -> Result<usize> {
    let io = v.details.target.as_ref().and_then(|v| v.io.as_ref());
    for test in &v.tests {
        if let Some(shape) = v.details.shape {
            if test.validation.is_some() != shape.validator
                || test.judges.len() != usize::from(shape.judges)
            {
                return Err(Error("judge evidence does not match planned shape"));
            }
        } else if test.validation.is_some() || !test.judges.is_empty() {
            return Err(Error("judged report has no planned shape"));
        }
    }
    // A file-I/O report is bounded by the shape its plan reserved for, never
    // by a shape inferred afterwards (that could exceed the reservation).
    if io.is_some() && v.details.shape.is_none() {
        return Err(Error("file-I/O report has no planned shape"));
    }
    if v.details
        .shape
        .is_some_and(|shape| shape.output_file != io.is_some_and(|io| io.output.is_some()))
    {
        return Err(Error("report shape does not match target I/O"));
    }
    match v.details.shape {
        Some(shape) => {
            shaped_report_bound(v.executions.len(), v.tests.len(), v.aggregates.len(), shape)
        }
        None => report_bound(v.executions.len(), v.tests.len(), v.aggregates.len()),
    }
}
fn report_value(v: &e::ReportData, persist: Option<&ReportPersistence>) -> Result<Value> {
    let meta = &v.details;
    let io = meta.target.as_ref().and_then(|v| v.io.as_ref());
    if io.is_some()
        && (v.command != crate::report::JobKind::Test
            || meta
                .problem
                .as_ref()
                .is_some_and(|v| v.kind == "interactive"))
    {
        return Err(Error("file I/O is only applied to test jobs"));
    }
    for test in &v.tests {
        if test.output_file.is_some() != io.is_some_and(|io| io.output.is_some()) {
            return Err(Error("output-file evidence does not match target I/O"));
        }
    }
    // The written `target.check` (default `pass_fail` with a comparator)
    // decides whether scores may appear (report §2.13).
    let pass_fail = meta.target.as_ref().is_some_and(|target| {
        target
            .check
            .as_ref()
            .map_or(target.compare.is_some(), |check| {
                check.scoring == crate::report::Scoring::PassFail
            })
    });
    if pass_fail && (meta.score.is_some() || v.tests.iter().any(|test| test.score.is_some())) {
        return Err(Error("pass_fail report has a score"));
    }
    if v.schema != 1 || v.document != "job_report" || v.outcome != "finished" {
        return Err(Error("expected finalized schema-1 job report"));
    }
    if v.cpf_version.len() > 32 || !v.cpf_version.is_ascii() {
        return Err(Error("invalid cpf version"));
    }
    if v.target.is_some() != meta.target.is_some() {
        return Err(Error("target metadata does not match engine"));
    }
    if meta
        .target
        .as_ref()
        .is_some_and(|target| target.compare != v.comparator)
    {
        return Err(Error("comparator metadata does not match engine"));
    }
    // The engine binding is authoritative; metadata adds only name and kind.
    match (&meta.problem, &v.problem) {
        (Some(meta), Some(binding))
            if meta.identity.strip_prefix("local:")
                == Some(binding.id.strip_prefix("local:").unwrap_or(&binding.id))
                && meta.dir == binding.dir => {}
        (None, None) => {}
        _ => return Err(Error("problem metadata does not match engine")),
    }
    if (v.command == crate::report::JobKind::Test) != meta.fact.is_some() {
        return Err(Error("test report needs fact outcome"));
    }
    if matches!(persist, Some(ReportPersistence::Refusal)) {
        return Err(Error("finished report cannot have refusal persistence"));
    }
    let mut warnings = Vec::new();
    let mut late_cancellation = None;
    for engine_warning_value in &v.warnings {
        let warning = engine_warning(engine_warning_value, v.result.cancellation)?;
        if matches!(engine_warning_value, e::Warning::LateCancellation) {
            late_cancellation = Some(warning);
        } else {
            warnings.push(warning);
        }
    }
    warnings.extend(meta.warnings.clone());
    warnings.extend(late_cancellation);
    let mut fields = vec![
        ("outcome", s("finished")),
        ("cpf_version", s(v.cpf_version)),
        ("command", s(v.command.wire_token())),
        ("job", job(&v.job, Some(&v.workspace))?),
        (
            "workspace",
            native::nsum(&native::native_bytes(v.workspace.as_os_str()), 1024),
        ),
        (
            "problem",
            meta.problem
                .as_ref()
                .map(|p| problem(p, Some(&v.workspace)))
                .transpose()?
                .unwrap_or(Value::Null),
        ),
        (
            "target",
            meta.target
                .as_ref()
                .map(target)
                .transpose()?
                .unwrap_or(Value::Null),
        ),
        (
            "capture",
            v.capture
                .as_ref()
                .map(capture)
                .transpose()?
                .unwrap_or(Value::Null),
        ),
        (
            "fingerprints",
            opt(meta.fingerprints.as_ref(), |(source, tests)| {
                obj([
                    ("source", hex(source)),
                    ("tests", hex(tests)),
                    ("judge", opt(meta.judge_fingerprint.as_ref(), |v| hex(v))),
                ])
            }),
        ),
        (
            "build",
            v.build
                .as_ref()
                .map(|build_value| build(build_value, &v.job.dir))
                .transpose()?
                .unwrap_or(Value::Null),
        ),
        ("roles", roles::roles(&meta.roles, false)?),
        (
            "executions",
            checked_array(&v.executions, 10_000, run::execution)?,
        ),
        ("tests", checked_array(&v.tests, 10_000, test)?),
        (
            "aggregates",
            checked_array(&v.aggregates, 10_000, aggregate)?,
        ),
        ("result", result(&v.result, meta)?),
        (
            "fact",
            meta.fact
                .as_ref()
                .map(fact)
                .transpose()?
                .unwrap_or(Value::Null),
        ),
        ("submission", Value::Null),
        (
            "hooks",
            Value::Array(
                meta.hooks
                    .iter()
                    .take(32)
                    .map(hook)
                    .collect::<Result<_>>()?,
            ),
        ),
        (
            "hooks_dropped",
            dropped(meta.hooks.len(), 32, meta.hooks_dropped),
        ),
        (
            "warnings",
            Value::Array(
                warnings
                    .iter()
                    .take(24)
                    .map(warning)
                    .collect::<Result<_>>()?,
            ),
        ),
        (
            "warnings_dropped",
            dropped(warnings.len(), 24, meta.warnings_dropped),
        ),
        ("events", obj([("dropped", u(v.events.dropped))])),
        ("timing", timing(&v.timing)?),
        ("refusal", Value::Null),
        ("stress", Value::Null),
    ];
    if let Some(persist) = persist {
        fields.push(("persistence", persistence(persist)?));
    }
    document("job_report", v.platform, fields)
}
/// Persisted report.json: deliberately excludes its own persistence outcome.
pub fn encode_report(report: &e::FinalizedReport) -> Result<Vec<u8>> {
    encode_report_data(report.data())
}
/// Also useful for fixtures and independent finalized-value producers.
pub fn encode_report_data(report: &e::ReportData) -> Result<Vec<u8>> {
    bounded(&report_value(report, None)?, report_data_bound(report)?)
}
/// stdout-only wrapper, also embedded by the final event envelope.
pub fn encode_stdout_report(report: &e::FinalizedReport) -> Result<Vec<u8>> {
    encode_stdout_report_data(report.data(), report.persistence())
}
pub fn encode_stdout_report_data(
    report: &e::ReportData,
    persistence: &ReportPersistence,
) -> Result<Vec<u8>> {
    bounded(
        &report_value(report, Some(persistence))?,
        report_data_bound(report)?,
    )
}

fn refusal_value(v: &Refusal) -> Result<Value> {
    if !matches!(
        v.class,
        FailureClass::Config
            | FailureClass::Workspace
            | FailureClass::Cancelled
            | FailureClass::Infrastructure
            | FailureClass::Internal
    ) {
        return Err(Error("invalid refusal class"));
    }
    if v.cpf_version.len() > 32 || !v.cpf_version.is_ascii() {
        return Err(Error("invalid cpf version"));
    }
    let result = obj([
        ("verdict", s("refused")),
        ("primary_verdict", s("refused")),
        ("partial", b(false)),
        (
            "cancellation",
            v.cancellation
                .map(cancellation)
                .transpose()?
                .unwrap_or(Value::Null),
        ),
        ("pre_delivery_exit", pre_exit(v.pre_delivery_exit)?),
        ("counts", counts(crate::report::Counts::default())),
        ("internal", Value::Null),
        ("score", Value::Null),
    ]);
    document(
        "job_report",
        v.platform,
        vec![
            ("outcome", s("refused")),
            ("cpf_version", s(&v.cpf_version)),
            ("command", s(v.command.wire_token())),
            (
                "job",
                v.job
                    .as_ref()
                    .map(|job_value| job(job_value, v.workspace.as_deref()))
                    .transpose()?
                    .unwrap_or(Value::Null),
            ),
            (
                "workspace",
                opt(v.workspace.as_ref(), |v| {
                    native::nsum(&native::native_bytes(v.as_os_str()), 1024)
                }),
            ),
            (
                "problem",
                v.problem
                    .as_ref()
                    .map(|p| problem(p, v.workspace.as_deref()))
                    .transpose()?
                    .unwrap_or(Value::Null),
            ),
            ("target", Value::Null),
            ("capture", Value::Null),
            ("fingerprints", Value::Null),
            ("build", Value::Null),
            ("roles", Value::Array(vec![])),
            ("executions", Value::Array(vec![])),
            ("tests", Value::Array(vec![])),
            ("aggregates", Value::Array(vec![])),
            ("result", result),
            ("fact", Value::Null),
            ("submission", Value::Null),
            ("hooks", Value::Array(vec![])),
            ("hooks_dropped", n(0)),
            (
                "warnings",
                Value::Array(
                    v.warnings
                        .iter()
                        .take(24)
                        .map(warning)
                        .collect::<Result<_>>()?,
                ),
            ),
            (
                "warnings_dropped",
                dropped(v.warnings.len(), 24, v.warnings_dropped),
            ),
            ("events", obj([("dropped", u(v.events_dropped))])),
            ("timing", timing(&v.timing)?),
            (
                "refusal",
                obj([
                    ("class", s(v.class.token())),
                    ("error", document_error(&v.error)?),
                    (
                        "diagnostics",
                        array(v.diagnostics.iter().take(64), diagnostic),
                    ),
                    (
                        "diagnostics_dropped",
                        dropped(v.diagnostics.len(), 64, v.diagnostics_dropped),
                    ),
                ]),
            ),
            ("stress", Value::Null),
            ("persistence", persistence(&ReportPersistence::Refusal)?),
        ],
    )
}
pub fn encode_refusal(v: &Refusal) -> Result<Vec<u8>> {
    bounded(&refusal_value(v)?, REFUSAL_BOUND)
}
pub fn encode_error(v: &ErrorDocument) -> Result<Vec<u8>> {
    bounded(
        &document(
            "error",
            v.platform,
            vec![
                ("command", token(&v.command)?),
                ("class", s(v.class.token())),
                ("exit", code(v.exit)?),
                ("error", document_error(&v.error)?),
                (
                    "diagnostics",
                    array(v.diagnostics.iter().take(64), diagnostic),
                ),
                (
                    "diagnostics_dropped",
                    dropped(v.diagnostics.len(), 64, v.diagnostics_dropped),
                ),
            ],
        )?,
        REFUSAL_BOUND,
    )
}

/// Complete, uncapped capture lists. Publication and reservation are separate.
pub fn encode_capture_manifest(
    platform: &str,
    job: &crate::storage::job::JobId,
    check: &crate::jobs::CaptureCheck,
    sources: &[std::path::PathBuf],
    support_files: &[std::path::PathBuf],
    lists_sha256: &[u8; 32],
    roles: &[crate::report::CaptureRole],
) -> Result<Vec<u8>> {
    // The report's `roles` order (§4.1), one entry per role.
    if roles.windows(2).any(|pair| pair[0].role >= pair[1].role) {
        return Err(Error("capture roles out of order or duplicated"));
    }
    Ok(encode(&document(
        "capture_manifest",
        platform,
        vec![
            ("job", s(job.as_str())),
            ("check", capture_check(check)?),
            (
                "sources",
                array(sources, |v| {
                    native::ns(&native::native_bytes(v.as_os_str()))
                }),
            ),
            (
                "support_files",
                array(support_files, |v| {
                    native::ns(&native::native_bytes(v.as_os_str()))
                }),
            ),
            ("lists_sha256", hex(lists_sha256)),
            (
                "roles",
                checked_array(roles, 5, |v| -> Result<Value> {
                    Ok(obj([
                        ("role", s(v.role.wire_token())),
                        (
                            "files",
                            array(&v.files, |v| {
                                native::ns(&native::native_bytes(v.as_os_str()))
                            }),
                        ),
                    ]))
                })?,
            ),
        ],
    )?))
}

#[cfg(test)]
mod bounds;
#[cfg(test)]
mod io_tests;
#[cfg(test)]
mod judging_tests;
#[cfg(test)]
mod roles_tests;
#[cfg(test)]
mod tests;
