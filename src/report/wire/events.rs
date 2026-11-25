use super::{model::*, run::*, schema::*, value::*};
use crate::jobs::engine as e;

pub fn encode_event_header(
    platform: &str,
    command: crate::report::JobKind,
    cpf_version: &str,
    output: bool,
) -> Result<Vec<u8>> {
    if cpf_version.len() > 32 || !cpf_version.is_ascii() {
        return Err(Error("invalid cpf version"));
    }
    bounded(
        &document(
            "event_stream",
            platform,
            vec![
                ("command", s(command.wire_token())),
                ("cpf_version", s(cpf_version)),
                ("output", b(output)),
            ],
        )?,
        super::EVENT_BOUND,
    )
}
pub fn encode_event(v: &e::Event) -> Result<Vec<u8>> {
    let (name, mut fields) = match &v.payload {
        e::EventPayload::JobStarted {
            job,
            kind,
            sequence: (epoch, n),
        } => (
            "job_started",
            vec![
                ("job", s(job.as_str())),
                ("kind", s(kind.wire_token())),
                ("sequence", sequence(epoch, *n)),
            ],
        ),
        e::EventPayload::RunStarted {
            run,
            execution,
            stage: v_stage,
            role: v_role,
            digest,
        } => (
            "run_started",
            vec![
                ("run", run_id(run.as_str())?),
                ("execution", n(*execution)),
                ("stage", stage(*v_stage)),
                ("role", role(v_role)?),
                (
                    "digest",
                    hex(digest
                        .as_ref()
                        .ok_or(Error("run_started needs spec digest"))?),
                ),
            ],
        ),
        e::EventPayload::OutputChunk { run, stream, bytes } => {
            if bytes.len() > 4096 {
                return Err(Error("output chunk exceeds bound"));
            }
            (
                "output_chunk",
                vec![
                    ("run", run_id(run.as_str())?),
                    ("stream", s(stream_token(*stream))),
                    ("bytes", super::native::raw(bytes)),
                ],
            )
        }
        e::EventPayload::RunFinished { run, summary: v } => (
            "run_finished",
            vec![("run", run_id(run.as_str())?), ("summary", summary(v)?)],
        ),
        e::EventPayload::Progress { done, total } => {
            ("progress", vec![("done", n(*done)), ("total", n(*total))])
        }
    };
    fields.insert(0, ("seq", u(v.seq)));
    fields.insert(0, ("event", s(name)));
    bounded(&Value::Object(fields), super::EVENT_BOUND)
}
/// Terminal envelope reflects the engine's observed report publication.
pub fn encode_job_finished(v: &e::JobFinished) -> Result<Vec<u8>> {
    let path = match v.report.persistence() {
        ReportPersistence::Published { path, .. } if v.report_id.is_some() => Some(path.as_path()),
        _ => None,
    };
    job_finished(v.seq, &v.job, v.verdict, path, v.dropped)
}
pub fn job_finished(
    seq: u64,
    job: &crate::storage::job::JobId,
    verdict: crate::report::JobVerdict,
    path: Option<&std::path::Path>,
    dropped: u64,
) -> Result<Vec<u8>> {
    bounded(
        &obj([
            ("event", s("job_finished")),
            ("seq", u(seq)),
            ("job", s(job.as_str())),
            ("verdict", s(verdict.wire_token())),
            (
                "report",
                path.map(|v| rel(v, 60).map(|v| obj([("path", v)])))
                    .transpose()?
                    .unwrap_or(Value::Null),
            ),
            ("dropped", u(dropped)),
        ]),
        super::EVENT_BOUND,
    )
}
pub fn encode_final_report(v: &e::FinalizedReport) -> Result<Vec<u8>> {
    let report = super::report_value(v.data(), Some(v.persistence()))?;
    bounded(
        &obj([("event", s("report")), ("report", report)]),
        super::report_data_bound(v.data())? + 28,
    )
}
pub fn encode_final_refusal(v: &Refusal) -> Result<Vec<u8>> {
    bounded(
        &obj([("event", s("report")), ("report", super::refusal_value(v)?)]),
        super::REFUSAL_BOUND + 28,
    )
}
