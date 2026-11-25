//! Role snapshots, including lossless bounded previews, in Appendix A order.
use super::{
    judging::{read_text, serialized, text as summary},
    schema::*,
    value::*,
};
use crate::report as r;
use serde_json::Value as Json;

fn previews(v: &[r::JudgeText]) -> Result<Value> {
    checked_array(v, 16, |v| summary(v, 128, false))
}
fn header(v: &r::Header) -> Result<Value> {
    Ok(match v {
        r::Header::Candidate {
            path,
            sha256,
            pinned,
        } => obj([
            ("kind", s("candidate")),
            ("path", summary(path, 128, false)?),
            ("sha256", hex(sha256)),
            ("pinned", b(*pinned)),
        ]),
        r::Header::NotIdentified { reason } => obj([
            ("kind", s("not_identified")),
            ("reason", serialized(reason)?),
        ]),
        r::Header::NotApplicable => kind("not_applicable"),
    })
}
fn capture(v: &r::RoleCapture) -> Result<Value> {
    if u64::from(v.counts[0]) != v.sources.len() as u64 + u64::from(v.sources_dropped)
        || u64::from(v.counts[1])
            != v.support_files.len() as u64 + u64::from(v.support_files_dropped)
    {
        return Err(Error("role capture counts do not match previews"));
    }
    Ok(obj([
        (
            "check",
            if v.checked {
                kind("checked")
            } else {
                obj([
                    ("kind", s("unchecked")),
                    ("reasons", Value::Array(vec![s("shell_stage")])),
                ])
            },
        ),
        ("sources", previews(&v.sources)?),
        ("sources_dropped", n(v.sources_dropped)),
        ("support_files", previews(&v.support_files)?),
        ("support_files_dropped", n(v.support_files_dropped)),
        (
            "counts",
            obj([
                ("sources", n(v.counts[0])),
                ("support_files", n(v.counts[1])),
            ]),
        ),
        ("bytes", u(v.bytes)),
        ("lists_sha256", hex(&v.lists_sha256)),
        ("content_sha256", hex(&v.content_sha256)),
    ]))
}
fn build_cause(v: &r::RoleBuildCause) -> Result<Value> {
    Ok(match v {
        r::RoleBuildCause::Run(v) => {
            // §2.3: a failed build's class cause is a deadline, budget,
            // signal or non-zero exit; a zero exit fails only by its checks.
            if matches!(
                v,
                r::RunClass::Infrastructure { .. }
                    | r::RunClass::Cancelled
                    | r::RunClass::ExitedZero
            ) || matches!(v, r::RunClass::OutputBudget { streams } if streams.output_file)
            {
                return Err(Error("invalid role build cause"));
            }
            super::run::class(v)?
        }
        r::RoleBuildCause::MissingOutput {
            names,
            names_dropped,
        } => obj([
            ("kind", s("missing_output")),
            ("names", previews(names)?),
            ("names_dropped", n(*names_dropped)),
        ]),
        r::RoleBuildCause::GeneratedProgramInvalid { name, reason } => obj([
            ("kind", s("generated_program_invalid")),
            ("name", summary(name, 128, false)?),
            ("reason", text(reason)),
        ]),
        r::RoleBuildCause::OutputTooLarge { bytes, cap } => obj([
            ("kind", s("output_too_large")),
            ("bytes", u(*bytes)),
            ("cap", u(*cap)),
        ]),
    })
}
fn build(v: &r::RoleBuild) -> Result<Value> {
    use r::RoleBuildResult as B;
    // `build_id` and `output_root` are null iff the build was skipped; a
    // build skipped for cancellation reports the result `cancelled` (§2.3).
    let unstarted = match v.result {
        B::Skipped { .. } => Some(true),
        B::Cancelled => None,
        _ => Some(false),
    };
    if v.build_id.is_none() != v.output_root.is_none()
        || unstarted.is_some_and(|skipped| skipped != v.build_id.is_none())
        || (!matches!(v.result, B::Succeeded)
            && (!v.artifacts.is_empty() || v.artifacts_dropped != 0))
    {
        return Err(Error("invalid role build evidence"));
    }
    let result = match &v.result {
        B::Succeeded => kind("succeeded"),
        B::Cancelled => kind("cancelled"),
        B::Infrastructure { causes } => obj([
            ("kind", s("infrastructure")),
            ("causes", super::run::causes(causes)),
            ("causes_dropped", n(causes.causes_dropped)),
        ]),
        B::Failed { cause } => obj([("kind", s("failed")), ("cause", build_cause(cause)?)]),
        B::InternalError { invariant } => obj([
            ("kind", s("internal_error")),
            ("invariant", s(invariant.wire_token())),
        ]),
        B::Skipped { reason } => {
            if !matches!(
                reason,
                r::SkipReason::BuildNotSucceeded
                    | r::SkipReason::AdmissionClosed
                    | r::SkipReason::SubmitNotSucceeded
            ) {
                return Err(Error("invalid role build skip reason"));
            }
            obj([("kind", s("skipped")), ("reason", s(reason.wire_token()))])
        }
    };
    if let B::Infrastructure { causes } = &v.result {
        if causes.as_slice().is_empty()
            || causes.as_slice().iter().any(|v| {
                matches!(
                    v,
                    r::InfrastructureCause::CaptureLoss {
                        stream: r::Stream::OutputFile,
                        ..
                    }
                )
            })
        {
            return Err(Error("invalid role build infrastructure"));
        }
    }
    let id = v
        .build_id
        .as_deref()
        .map(run_id)
        .transpose()?
        .unwrap_or(Value::Null);
    if let (Some(id), Some(root)) = (&v.build_id, &v.output_root) {
        if *root != format!("builds/{id}") {
            return Err(Error("role build root does not match id"));
        }
    }
    let value = obj([
        ("execution", n(v.execution)),
        ("build_id", id),
        (
            "output_root",
            v.output_root
                .as_ref()
                .map(|v| rel(std::path::Path::new(v), 15))
                .transpose()?
                .unwrap_or(Value::Null),
        ),
        ("result", result),
        ("artifacts", previews(&v.artifacts)?),
        ("artifacts_dropped", n(v.artifacts_dropped)),
    ]);
    bounded(&value, 6664)?;
    Ok(value)
}
pub(super) fn role(v: &r::RoleReport) -> Result<Value> {
    let judge = matches!(
        v.role,
        r::RoleName::Checker | r::RoleName::Validator | r::RoleName::Interactor
    );
    if judge != v.profile.is_some() {
        return Err(Error("role profile does not match role"));
    }
    let (origin, provenance) = match (&v.origin, &v.provenance) {
        (r::RoleOrigin::Built { language, entry }, r::RoleProvenance::Built { header: h }) => {
            if (v.profile == Some(r::JudgeProfile::Testlib))
                == matches!(h, r::Header::NotApplicable)
            {
                return Err(Error("header provenance does not match profile"));
            }
            (
                obj([
                    ("kind", s("built")),
                    ("language", ident(language)?),
                    (
                        "entry",
                        entry
                            .as_ref()
                            .map(|v| summary(v, 128, false))
                            .transpose()?
                            .unwrap_or(Value::Null),
                    ),
                ]),
                obj([("kind", s("built")), ("header", header(h)?)]),
            )
        }
        (r::RoleOrigin::Prebuilt { program }, r::RoleProvenance::Declared) if v.build.is_none() => {
            (
                obj([
                    ("kind", s("prebuilt")),
                    ("program", summary(program, 128, false)?),
                ]),
                kind("declared"),
            )
        }
        _ => return Err(Error("role origin and provenance disagree")),
    };
    let value = obj([
        ("role", s(v.role.wire_token())),
        ("profile", serialized(&v.profile)?),
        ("origin", origin),
        (
            "build",
            v.build
                .as_ref()
                .map(build)
                .transpose()?
                .unwrap_or(Value::Null),
        ),
        ("capture", capture(&v.capture)?),
        ("provenance", provenance),
    ]);
    bounded(&value, 21 * 1024)?;
    Ok(value)
}
/// §4.1: at most one entry per role, in the order checker, validator,
/// interactor, then generator and brute, which only a stress job has.
pub(super) fn roles(v: &[r::RoleReport], stress: bool) -> Result<Value> {
    if v.windows(2).any(|pair| pair[0].role >= pair[1].role) {
        return Err(Error("roles out of order or duplicated"));
    }
    if !stress
        && v.iter()
            .any(|v| matches!(v.role, r::RoleName::Generator | r::RoleName::Brute))
    {
        return Err(Error("generator and brute roles belong to stress jobs"));
    }
    checked_array(v, 5, role)
}

fn member<'a>(v: &'a Json, key: &str) -> Result<&'a Json> {
    v.get(key).ok_or(Error("missing role member"))
}
fn string(v: &Json) -> Result<&str> {
    v.as_str().ok_or(Error("expected role string"))
}
fn tag(v: &Json) -> Result<&str> {
    string(member(v, "kind")?)
}
fn parsed<T: serde::de::DeserializeOwned>(v: &Json) -> Result<T> {
    serde_json::from_value(v.clone()).map_err(|_| Error("invalid role enum"))
}
fn optional<T>(v: &Json, f: impl FnOnce(&Json) -> Result<T>) -> Result<Option<T>> {
    if v.is_null() {
        Ok(None)
    } else {
        f(v).map(Some)
    }
}
fn boolean(v: &Json) -> Result<bool> {
    v.as_bool().ok_or(Error("expected role bool"))
}
fn previews_read(v: &Json) -> Result<Vec<r::JudgeText>> {
    v.as_array()
        .ok_or(Error("expected role previews"))?
        .iter()
        .map(|v| read_text(v, 128, false))
        .collect()
}
pub(super) fn read_hash(v: &Json) -> Result<[u8; 32]> {
    let value = string(v)?;
    if value.len() != 64
        || !value
            .bytes()
            .all(|v| v.is_ascii_digit() || (b'a'..=b'f').contains(&v))
    {
        return Err(Error("invalid role digest"));
    }
    let mut out = [0; 32];
    for (i, b) in out.iter_mut().enumerate() {
        *b = u8::from_str_radix(&value[2 * i..2 * i + 2], 16)
            .map_err(|_| Error("invalid role digest"))?;
    }
    Ok(out)
}
fn read_token<T>(v: &Json) -> Result<T>
where
    for<'a> T: TryFrom<&'a str, Error = ()>,
{
    T::try_from(string(v)?).map_err(|_| Error("unknown role token"))
}
fn read_capture(v: &Json) -> Result<r::RoleCapture> {
    let check = member(v, "check")?;
    let checked = match tag(check)? {
        "checked" => true,
        "unchecked"
            if member(check, "reasons")?
                .as_array()
                .is_some_and(|v| v.len() == 1 && v[0].as_str() == Some("shell_stage")) =>
        {
            false
        }
        _ => return Err(Error("invalid capture check")),
    };
    let counts = member(v, "counts")?;
    Ok(r::RoleCapture {
        checked,
        sources: previews_read(member(v, "sources")?)?,
        sources_dropped: super::read::read_u32(member(v, "sources_dropped")?)?,
        support_files: previews_read(member(v, "support_files")?)?,
        support_files_dropped: super::read::read_u32(member(v, "support_files_dropped")?)?,
        counts: [
            super::read::read_u32(member(counts, "sources")?)?,
            super::read::read_u32(member(counts, "support_files")?)?,
        ],
        bytes: super::read::read_u64(member(v, "bytes")?)?,
        lists_sha256: read_hash(member(v, "lists_sha256")?)?,
        content_sha256: read_hash(member(v, "content_sha256")?)?,
    })
}
fn read_causes(v: &Json, dropped: &Json) -> Result<r::InfrastructureCauses> {
    let array = v.as_array().ok_or(Error("expected causes"))?;
    if array.is_empty() || array.len() > 8 {
        return Err(Error("invalid causes"));
    }
    let mut causes = Vec::new();
    for v in array {
        causes.push(match tag(v)? {
            "not_started" => r::InfrastructureCause::NotStarted {
                reason: read_token(member(v, "reason")?)?,
            },
            "status_unknown" => r::InfrastructureCause::StatusUnknown,
            "cleanup_incomplete" => r::InfrastructureCause::CleanupIncomplete {
                survivors: super::read::read_u32(member(v, "survivors")?)?,
            },
            "supervisor_error" => r::InfrastructureCause::SupervisorError {
                error: read_token(member(v, "error")?)?,
            },
            "capture_loss" => r::InfrastructureCause::CaptureLoss {
                stream: read_token(member(v, "stream")?)?,
                loss: read_token(member(v, "loss")?)?,
            },
            _ => return Err(Error("unknown infrastructure cause")),
        });
    }
    let mut out = r::InfrastructureCauses::new(causes.clone());
    if out.as_slice() != causes.as_slice() {
        return Err(Error("invalid cause order"));
    }
    out.causes_dropped = super::read::read_u32(dropped)?;
    Ok(out)
}
fn read_class(v: &Json) -> Result<r::RunClass> {
    Ok(match tag(v)? {
        "exited_zero" => r::RunClass::ExitedZero,
        "wall_deadline" => r::RunClass::WallDeadline,
        "nonzero_exit" => r::RunClass::NonzeroExit {
            code: member(v, "code")?
                .as_i64()
                .ok_or(Error("invalid exit code"))?,
        },
        "signal" => {
            member(v, "name")?;
            r::RunClass::Signal {
                signal: member(v, "signal")?
                    .as_i64()
                    .and_then(|v| v.try_into().ok())
                    .ok_or(Error("invalid signal"))?,
                core_dumped: boolean(member(v, "core_dumped")?)?,
            }
        }
        "output_budget" => r::RunClass::OutputBudget {
            streams: super::read::read_budget_streams(v)?,
        },
        _ => return Err(Error("invalid role build class")),
    })
}
fn read_build_cause(v: &Json) -> Result<r::RoleBuildCause> {
    Ok(match tag(v)? {
        "missing_output" => r::RoleBuildCause::MissingOutput {
            names: previews_read(member(v, "names")?)?,
            names_dropped: super::read::read_u32(member(v, "names_dropped")?)?,
        },
        "generated_program_invalid" => r::RoleBuildCause::GeneratedProgramInvalid {
            name: read_text(member(v, "name")?, 128, false)?,
            reason: {
                let reason = string(member(v, "reason")?)?;
                if encode(&s(reason)).len() > 260 {
                    return Err(Error("build reason exceeds bound"));
                }
                reason.into()
            },
        },
        "output_too_large" => r::RoleBuildCause::OutputTooLarge {
            bytes: super::read::read_u64(member(v, "bytes")?)?,
            cap: super::read::read_u64(member(v, "cap")?)?,
        },
        _ => r::RoleBuildCause::Run(read_class(v)?),
    })
}
fn read_build(v: &Json) -> Result<r::RoleBuild> {
    use r::RoleBuildResult as B;
    let result = member(v, "result")?;
    let result = match tag(result)? {
        "succeeded" => B::Succeeded,
        "cancelled" => B::Cancelled,
        "infrastructure" => B::Infrastructure {
            causes: read_causes(member(result, "causes")?, member(result, "causes_dropped")?)?,
        },
        "failed" => B::Failed {
            cause: Box::new(read_build_cause(member(result, "cause")?)?),
        },
        "internal_error" => B::InternalError {
            invariant: read_token(member(result, "invariant")?)?,
        },
        "skipped" => B::Skipped {
            reason: read_token(member(result, "reason")?)?,
        },
        _ => return Err(Error("unknown role build result")),
    };
    Ok(r::RoleBuild {
        execution: super::read::read_u32(member(v, "execution")?)?,
        build_id: optional(member(v, "build_id")?, |v| -> Result<String> {
            Ok(string(v)?.to_owned())
        })?,
        output_root: optional(member(v, "output_root")?, |v| -> Result<String> {
            Ok(string(v)?.to_owned())
        })?,
        result,
        artifacts: previews_read(member(v, "artifacts")?)?,
        artifacts_dropped: super::read::read_u32(member(v, "artifacts_dropped")?)?,
    })
}
pub fn read_role(v: &Json) -> Result<r::RoleReport> {
    let origin = member(v, "origin")?;
    let origin = match tag(origin)? {
        "built" => r::RoleOrigin::Built {
            language: string(member(origin, "language")?)?.into(),
            entry: optional(member(origin, "entry")?, |v| read_text(v, 128, false))?,
        },
        "prebuilt" => r::RoleOrigin::Prebuilt {
            program: read_text(member(origin, "program")?, 128, false)?,
        },
        _ => return Err(Error("unknown role origin")),
    };
    let provenance = member(v, "provenance")?;
    let provenance = match tag(provenance)? {
        "declared" => r::RoleProvenance::Declared,
        "built" => {
            let h = member(provenance, "header")?;
            r::RoleProvenance::Built {
                header: match tag(h)? {
                    "candidate" => r::Header::Candidate {
                        path: read_text(member(h, "path")?, 128, false)?,
                        sha256: read_hash(member(h, "sha256")?)?,
                        pinned: boolean(member(h, "pinned")?)?,
                    },
                    "not_identified" => r::Header::NotIdentified {
                        reason: parsed(member(h, "reason")?)?,
                    },
                    "not_applicable" => r::Header::NotApplicable,
                    _ => return Err(Error("unknown header provenance")),
                },
            }
        }
        _ => return Err(Error("unknown role provenance")),
    };
    let out = r::RoleReport {
        role: parsed(member(v, "role")?)?,
        profile: optional(member(v, "profile")?, parsed)?,
        origin,
        build: optional(member(v, "build")?, read_build)?,
        capture: read_capture(member(v, "capture")?)?,
        provenance,
    };
    role(&out)?;
    Ok(out)
}
pub fn read_roles(v: &Json, stress: bool) -> Result<Vec<r::RoleReport>> {
    let out = v
        .as_array()
        .ok_or(Error("expected roles array"))?
        .iter()
        .map(read_role)
        .collect::<Result<Vec<_>>>()?;
    roles(&out, stress)?;
    Ok(out)
}

pub fn read_judge_warning(v: &Json) -> Result<super::Warning> {
    let kind = match tag(v)? {
        "judge_scratch" => super::WarningKind::JudgeScratch {
            run: string(member(v, "run")?)?.into(),
            detail: parsed(member(v, "detail")?)?,
        },
        "testlib_header" => super::WarningKind::TestlibHeader {
            role: parsed(member(v, "role")?)?,
            detail: parsed(member(v, "detail")?)?,
        },
        _ => return Err(Error("unknown judge warning")),
    };
    let message = string(member(v, "message")?)?;
    if encode(&s(message)).len() > 260 {
        return Err(Error("warning message exceeds bound"));
    }
    let out = super::Warning {
        kind,
        message: message.into(),
    };
    super::schema::warning(&out)?;
    Ok(out)
}
