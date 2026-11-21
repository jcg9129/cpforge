//! E §2 fixes native spec encoding, but E supplies no JSON member/variant
//! spellings for spec or record. These schema-1 documents use its declaration
//! order and the report vocabulary. The binary spec digest is independent.
use super::{native::*, run::*, schema::*, value::*};
use crate::jobs::engine as e;
use crate::supervisor as p;
use std::ffi::OsString;
use std::path::PathBuf;
use std::time::Duration;

#[derive(Clone, Debug)]
pub enum SpecStdin {
    Null,
    File(PathBuf),
    Forward,
}
#[derive(Clone, Debug)]
pub struct Spec {
    pub platform: &'static str,
    pub stage: e::Stage,
    pub program: PathBuf,
    pub args: Vec<OsString>,
    pub cwd: PathBuf,
    pub env: Vec<(OsString, OsString)>,
    pub stdin: SpecStdin,
    pub stdout: p::StreamPolicy,
    pub stderr: p::StreamPolicy,
    pub wall_deadline: Duration,
    pub limits: Vec<p::LocalLimit>,
    pub measure: Vec<p::MetricKind>,
}
fn policy(v: &p::StreamPolicy) -> Result<Value> {
    let sink = match &v.sink {
        p::Sink::File(path) => obj([
            ("kind", s("file")),
            ("path", ns(&native_bytes(path.as_os_str()))),
        ]),
        p::Sink::Forward(stream) => obj([
            ("kind", s("forward")),
            (
                "stream",
                s(match stream {
                    p::Stream::Stdout => "stdout",
                    p::Stream::Stderr => "stderr",
                }),
            ),
        ]),
        p::Sink::Discard => kind("discard"),
        p::Sink::Memory => return Err(Error("memory sink has no execution-contract wire variant")),
    };
    Ok(obj([
        ("sink", sink),
        ("budget", u(v.budget)),
        (
            "on_exceed",
            s(match v.on_exceed {
                p::Exceed::Kill => "kill",
                p::Exceed::Discard => "discard",
            }),
        ),
        ("events", b(v.events)),
    ]))
}
pub fn encode_spec(v: &Spec) -> Result<Vec<u8>> {
    if v.wall_deadline.is_zero() {
        return Err(Error("spec deadline must be positive"));
    }
    bounded(
        &document(
            "execution_spec",
            v.platform,
            vec![
                ("stage", stage(v.stage)),
                ("program", ns(&native_bytes(v.program.as_os_str()))),
                ("args", array(&v.args, |v| ns(&native_bytes(v)))),
                ("cwd", ns(&native_bytes(v.cwd.as_os_str()))),
                (
                    "env",
                    array(&v.env, |(name, value)| {
                        obj([
                            ("name", ns(&native_bytes(name))),
                            ("value", ns(&native_bytes(value))),
                        ])
                    }),
                ),
                (
                    "stdin",
                    match &v.stdin {
                        SpecStdin::Null => kind("null"),
                        SpecStdin::File(path) => obj([
                            ("kind", s("file")),
                            ("path", ns(&native_bytes(path.as_os_str()))),
                        ]),
                        SpecStdin::Forward => kind("forward"),
                    },
                ),
                ("stdout", policy(&v.stdout)?),
                ("stderr", policy(&v.stderr)?),
                ("wall_deadline_us", dur(v.wall_deadline)?),
                ("deadline_group", Value::Null),
                (
                    "limits",
                    checked_array(&v.limits, 2, |v| {
                        let (kind, bytes) = local(*v);
                        Ok(obj([("kind", s(kind)), ("bytes", u(bytes))]))
                    })?,
                ),
                (
                    "measure",
                    checked_array(&v.measure, 5, |v| Ok(s(metric(*v))))?,
                ),
            ],
        )?,
        256 * 1024,
    )
}
/// Full available immutable supervisor evidence; relative instants use the
/// creation-completed origin, never wall-clock or debug representations.
#[derive(Clone, Debug)]
pub struct Record<'a> {
    pub platform: &'static str,
    pub spec: &'a e::ArtifactRef,
    pub execution: &'a e::ExecutionRecord,
}
pub fn encode_record(v: &Record<'_>) -> Result<Vec<u8>> {
    let spec = obj([
        (
            "digest",
            hex(v
                .spec
                .digest
                .as_ref()
                .ok_or(Error("record needs spec digest"))?),
        ),
        (
            "path",
            v.spec
                .path
                .as_deref()
                .map(|v| rel(v, 18))
                .transpose()?
                .unwrap_or(Value::Null),
        ),
    ]);
    let (execution, stdout, stderr, measurements, limits, errors, notes) = match v.execution {
        e::ExecutionRecord::NotStarted(failure) => (
            obj([
                ("kind", s("not_started")),
                ("reason", not_started(failure)?),
            ]),
            stream(None, None)?,
            stream(None, None)?,
            Value::Array(vec![]),
            Value::Array(vec![]),
            Value::Array(vec![]),
            Value::Array(vec![]),
        ),
        e::ExecutionRecord::Started(root) => {
            let mut execution = match started(root)? {
                Value::Object(v) => v,
                _ => unreachable!(),
            };
            execution.push(("timing", super::run::timing(root)?));
            execution.push((
                "termination",
                obj([
                    (
                        "requested_at_us",
                        root.termination
                            .requested_at
                            .map(|v| dur(v.saturating_duration_since(root.timing.create_end)))
                            .transpose()?
                            .unwrap_or(Value::Null),
                    ),
                    (
                        "method",
                        root.termination
                            .method
                            .map(supervisor_token)
                            .transpose()?
                            .unwrap_or(Value::Null),
                    ),
                ]),
            ));
            let error_values = root
                .errors
                .iter()
                .map(|error| {
                    let (error, os_error) = match error {
                        p::SupervisorError::Register(v) => ("register", Some(*v)),
                        p::SupervisorError::Wait(v) => ("wait", Some(*v)),
                        p::SupervisorError::Kill(v) => ("kill", Some(*v)),
                        p::SupervisorError::Reap => ("reap", None),
                        p::SupervisorError::ReapTimeout => ("reap_timeout", None),
                    };
                    obj([("kind", s(error)), ("os_error", opt(os_error, i))])
                })
                .collect::<Vec<_>>();
            if error_values.len() > 16 {
                return Err(Error("too many record supervisor errors"));
            }
            let note_values = root
                .notes
                .iter()
                .map(|v| match *v {
                    p::Note::LateTrigger(trigger_value) => obj([
                        ("kind", s("late_trigger")),
                        ("trigger", trigger(trigger_value)),
                    ]),
                    v => kind(note(v)),
                })
                .collect::<Vec<_>>();
            if note_values.len() > 16 {
                return Err(Error("too many record notes"));
            }
            (
                Value::Object(execution),
                record_stream(
                    root.stdout.as_ref(),
                    root.stdout
                        .as_ref()
                        .and_then(|stream| stream.sink.as_deref()),
                )?,
                record_stream(
                    root.stderr.as_ref(),
                    root.stderr
                        .as_ref()
                        .and_then(|stream| stream.sink.as_deref()),
                )?,
                checked_array(&root.measurements, 5, |v| measurement(v, true))?,
                checked_array(&root.limits, 2, |v| limit(v, true))?,
                Value::Array(error_values),
                Value::Array(note_values),
            )
        }
    };
    bounded(
        &document(
            "run_record",
            v.platform,
            vec![
                ("spec", spec),
                ("execution", execution),
                ("stdout", stdout),
                ("stderr", stderr),
                ("measurements", measurements),
                ("limits", limits),
                ("supervisor_errors", errors),
                ("notes", notes),
            ],
        )?,
        64 * 1024,
    )
}
fn record_stream(v: Option<&p::StreamCapture>, file: Option<&std::path::Path>) -> Result<Value> {
    if v.is_none() && file.is_some() {
        return Err(Error("capture publication without capture evidence"));
    }
    let mut value = match stream(v, None)? {
        Value::Object(v) => v,
        _ => unreachable!(),
    };
    let complete = file.is_some() && v.is_some_and(p::StreamCapture::complete);
    value
        .iter_mut()
        .find(|(name, _)| *name == "complete")
        .expect("stream completeness member")
        .1 = b(complete);
    let file = file
        .map(|v| {
            let value = ns(&native_bytes(v.as_os_str()));
            if encode(&value).len() > 4096 {
                return Err(Error("record path exceeds 4 KiB"));
            }
            Ok(value)
        })
        .transpose()?
        .unwrap_or(Value::Null);
    value.last_mut().expect("stream file member").1 = file;
    Ok(Value::Object(value))
}

/// C §10's global value mapping, preserving effective table key order.
/// Native leaves are explicit because their meaning cannot be inferred from
/// TOML strings. No inherited process environment is read by this encoder.
#[derive(Clone, Debug)]
pub enum ConfigValue {
    String(String),
    Integer(i64),
    /// Metadata coordinates, rather than a TOML integer.
    U32(u32),
    Float(f64),
    Boolean(bool),
    Array(Vec<ConfigValue>),
    Table(Vec<(String, ConfigValue)>),
    Native(OsString),
}
pub fn encode_config_show(platform: &str, value: &ConfigValue) -> Result<Vec<u8>> {
    // Effective keys are dynamic, so compose the validated mapping through its
    // own ordered writer, after the shared fixed document prefix.
    let mut bytes = encode(&document("config_show", platform, vec![])?);
    bytes.pop();
    bytes.extend_from_slice(b",\"config\":");
    config(value, &mut bytes)?;
    bytes.push(b'}');
    Ok(bytes)
}
/// Inspection metadata follows the configuration mapping, using the same
/// canonical writer. The schema permits additive members (§3.6).
pub fn encode_config_inspection(
    platform: &str,
    value: &ConfigValue,
    metadata: &[(String, ConfigValue)],
) -> Result<Vec<u8>> {
    let mut bytes = encode_config_show(platform, value)?;
    bytes.pop();
    let mut seen = std::collections::BTreeSet::from(["schema", "document", "platform", "config"]);
    for (key, value) in metadata {
        if !seen.insert(key.as_str()) {
            return Err(Error("duplicate configuration metadata key"));
        }
        bytes.push(b',');
        bytes.extend(encode(&s(key)));
        bytes.push(b':');
        config(value, &mut bytes)?;
    }
    bytes.push(b'}');
    Ok(bytes)
}

fn config(v: &ConfigValue, out: &mut Vec<u8>) -> Result<()> {
    match v {
        ConfigValue::String(v) => out.extend(encode(&s(v))),
        ConfigValue::Integer(v) => out.extend(encode(&s(v.to_string()))),
        ConfigValue::U32(v) => out.extend(encode(&n(*v))),
        ConfigValue::Float(v) => out.extend(encode(&float(*v))),
        ConfigValue::Boolean(v) => out.extend(encode(&b(*v))),
        ConfigValue::Native(v) => out.extend(encode(&ns(&native_bytes(v)))),
        ConfigValue::Array(v) => {
            out.push(b'[');
            for (j, v) in v.iter().enumerate() {
                if j > 0 {
                    out.push(b',');
                }
                config(v, out)?;
            }
            out.push(b']');
        }
        ConfigValue::Table(v) => {
            let mut seen = std::collections::BTreeSet::new();
            out.push(b'{');
            for (j, (key, v)) in v.iter().enumerate() {
                if !seen.insert(key) {
                    return Err(Error("duplicate configuration key"));
                }
                if j > 0 {
                    out.push(b',');
                }
                out.extend(encode(&s(key)));
                out.push(b':');
                config(v, out)?;
            }
            out.push(b'}');
        }
    }
    Ok(())
}
