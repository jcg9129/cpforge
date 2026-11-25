//! Batch judge evidence in Appendix A member order.
use super::value::*;
use crate::report as r;

pub(super) fn serialized<T: serde::Serialize>(v: &T) -> Result<Value> {
    serde_json::to_vec(v)
        .map(Value::Verbatim)
        .map_err(|_| Error("judging encoding failed"))
}
pub(super) fn config(v: &r::CheckConfig) -> Result<Value> {
    if v.kind == r::CheckMode::Compare && v.scoring != r::Scoring::PassFail {
        return Err(Error("comparison cannot use points scoring"));
    }
    if (v.kind == r::CheckMode::Interactive) != v.interactive.is_some() {
        return Err(Error("interactive config does not match check kind"));
    }
    r::test_score(v.scoring, &r::TestVerdict::Passed, None)
        .map_err(|_| Error("invalid scoring scale"))?;
    serialized(v)
}
pub(super) fn aggregate(v: &r::AggregateScore) -> Result<Value> {
    let json = serde_json::to_value(v).map_err(|_| Error("invalid aggregate score"))?;
    read_aggregate_score(&json)?;
    serialized(v)
}
pub(super) fn text(v: &r::JudgeText, cap: usize, utf8: bool) -> Result<Value> {
    if v.head.len() > cap
        || v.len < v.head.len() as u64
        || (utf8 && std::str::from_utf8(&v.head).is_err())
    {
        return Err(Error("invalid judge text summary"));
    }
    Ok(obj([
        ("head", super::native::ns(&v.head)),
        ("len", u(v.len)),
        ("truncated", b(v.len > v.head.len() as u64)),
        ("sha256", hex(&v.sha256)),
    ]))
}
pub(super) fn optional_text(v: Option<&r::JudgeText>, cap: usize, utf8: bool) -> Result<Value> {
    v.map(|v| text(v, cap, utf8))
        .transpose()
        .map(|v| v.unwrap_or(Value::Null))
}
pub(super) fn validation(v: &r::Validation) -> Result<Value> {
    let value = obj([
        ("execution", n(v.execution)),
        ("profile", serialized(&v.profile)?),
        ("outcome", serialized(&v.outcome)?),
        ("stderr", optional_text(v.stderr.as_ref(), 1024, false)?),
    ]);
    bounded(&value, 2560)?;
    Ok(value)
}
pub(super) fn judge(v: &r::Judge) -> Result<Value> {
    if let Some(r::JudgeResult::Scored { raw, scale }) = v.result {
        if raw > scale {
            return Err(Error("judge score exceeds scale"));
        }
    }
    if v.profile == r::JudgeProfile::Simple && (v.message.is_some() || v.report.is_some()) {
        return Err(Error("simple judge has testlib evidence"));
    }
    if matches!(
        v.result,
        Some(r::JudgeResult::NotRead {
            reason: r::JudgeNotRead::ExtractionFailed
        })
    ) && v.report.as_ref().is_some_and(|v| v.path.is_some())
    {
        return Err(Error("failed extraction names an extracted report"));
    }
    let report = v
        .report
        .as_ref()
        .map(|v| -> Result<Value> {
            if v.bytes > 65_537 || (v.bytes > 65_536 && v.path.is_some()) {
                return Err(Error("judge report exceeds read cap"));
            }
            let path = v
                .path
                .as_ref()
                .map(|p| -> Result<Value> {
                    let run = p
                        .strip_suffix("/judge/report")
                        .ok_or(Error("invalid judge report path"))?;
                    super::schema::run_id(run)?;
                    Ok(s(p))
                })
                .transpose()?
                .unwrap_or(Value::Null);
            Ok(obj([
                ("path", path),
                ("bytes", u(v.bytes)),
                ("encoding", serialized(&v.encoding)?),
            ]))
        })
        .transpose()?
        .unwrap_or(Value::Null);
    let value = obj([
        ("role", serialized(&v.role)?),
        ("execution", n(v.execution)),
        ("profile", serialized(&v.profile)?),
        ("result", serialized(&v.result)?),
        ("message", optional_text(v.message.as_ref(), 4096, true)?),
        ("stderr", optional_text(v.stderr.as_ref(), 1024, false)?),
        ("report", report),
    ]);
    bounded(&value, 11 * 1024)?;
    Ok(value)
}

type Json = serde_json::Value;
fn member<'a>(v: &'a Json, key: &str) -> Result<&'a Json> {
    v.get(key).ok_or(Error("missing judging member"))
}
fn parsed<T: serde::de::DeserializeOwned>(v: &Json) -> Result<T> {
    serde_json::from_value(v.clone()).map_err(|_| Error("invalid judging value"))
}
fn optional<T>(v: &Json, f: impl FnOnce(&Json) -> Result<T>) -> Result<Option<T>> {
    if v.is_null() {
        Ok(None)
    } else {
        f(v).map(Some)
    }
}
pub(super) fn read_text(v: &Json, cap: usize, utf8: bool) -> Result<r::JudgeText> {
    let native = member(v, "head")?;
    // One canonical `ns` form (report §§3.3, 3.5).
    let head = match (native.get("utf8"), native.get("bytes_b64")) {
        (Some(text), None) => {
            let text = text.as_str().ok_or(Error("invalid summary head"))?;
            if super::native::eligible(text.as_bytes()).is_none() {
                return Err(Error("invalid UTF-8 summary head"));
            }
            text.as_bytes().to_vec()
        }
        (None, Some(b64)) => {
            let bytes = super::decode_base64(b64.as_str().ok_or(Error("invalid summary head"))?)?;
            if super::native::eligible(&bytes).is_some() {
                return Err(Error("non-canonical summary head"));
            }
            bytes
        }
        _ => return Err(Error("invalid summary head")),
    };
    let len = super::read::read_u64(member(v, "len")?)?;
    let hash = member(v, "sha256")?
        .as_str()
        .ok_or(Error("invalid text hash"))?;
    if hash.len() != 64
        || !hash
            .bytes()
            .all(|v| v.is_ascii_digit() || (b'a'..=b'f').contains(&v))
    {
        return Err(Error("invalid text hash"));
    }
    let mut sha256 = [0; 32];
    for (i, byte) in sha256.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&hash[i * 2..i * 2 + 2], 16)
            .map_err(|_| Error("invalid text hash"))?;
    }
    if member(v, "truncated")?.as_bool() != Some(len > head.len() as u64) {
        return Err(Error("invalid text truncation"));
    }
    if len == head.len() as u64 {
        use sha2::{Digest, Sha256};
        if Sha256::digest(&head)[..] != sha256[..] {
            return Err(Error("summary hash does not cover its value"));
        }
    }
    let result = r::JudgeText { head, len, sha256 };
    text(&result, cap, utf8)?;
    Ok(result)
}
pub fn read_check_config(v: &Json) -> Result<r::CheckConfig> {
    for key in ["kind", "answer", "scoring", "display", "interactive"] {
        member(v, key)?;
    }
    if let Some(interactive) = v.get("interactive").filter(|v| !v.is_null()) {
        for key in [
            "checking",
            "mode",
            "idle_us",
            "early_reader_close",
            "interactor_allowance_us",
            "peer_allowance_us",
            "solution_seed",
            "interactor_seed",
        ] {
            member(interactive, key)?;
        }
    }
    let result = parsed(v)?;
    config(&result)?;
    Ok(result)
}
pub fn read_judge_result(v: &Json) -> Result<r::JudgeResult> {
    let result = parsed(v)?;
    if let r::JudgeResult::Scored { raw, scale } = result {
        if raw > scale {
            return Err(Error("judge score exceeds scale"));
        }
    }
    Ok(result)
}
pub fn read_judged_check(v: &Json) -> Result<r::JudgedCheck> {
    let result = parsed(v)?;
    if let r::JudgedCheck::Scored { raw, scale, .. } = result {
        if raw > scale {
            return Err(Error("check score exceeds scale"));
        }
    }
    Ok(result)
}
pub fn read_validation(v: &Json) -> Result<r::Validation> {
    let result = r::Validation {
        execution: super::read::read_u32(member(v, "execution")?)?,
        profile: parsed(member(v, "profile")?)?,
        outcome: parsed(member(v, "outcome")?)?,
        stderr: optional(member(v, "stderr")?, |v| read_text(v, 1024, false))?,
    };
    validation(&result)?;
    Ok(result)
}
pub fn read_judge(v: &Json) -> Result<r::Judge> {
    let result = r::Judge {
        role: parsed(member(v, "role")?)?,
        execution: super::read::read_u32(member(v, "execution")?)?,
        profile: parsed(member(v, "profile")?)?,
        result: optional(member(v, "result")?, read_judge_result)?,
        message: optional(member(v, "message")?, |v| read_text(v, 4096, true))?,
        stderr: optional(member(v, "stderr")?, |v| read_text(v, 1024, false))?,
        report: optional(member(v, "report")?, |v| {
            Ok(r::JudgeReport {
                path: parsed(member(v, "path")?)?,
                bytes: super::read::read_u64(member(v, "bytes")?)?,
                encoding: parsed(member(v, "encoding")?)?,
            })
        })?,
    };
    judge(&result)?;
    Ok(result)
}
pub fn read_aggregate_score(v: &Json) -> Result<r::AggregateScore> {
    let result = parsed(v)?;
    match result {
        r::AggregateScore::Points {
            value, max, full, ..
        } if value > max || full != (value == max) => return Err(Error("invalid aggregate score")),
        r::AggregateScore::Incomplete { missing: 0, .. } => {
            return Err(Error("incomplete score has no missing tests"));
        }
        _ => {}
    }
    Ok(result)
}
pub(super) fn validate_members(v: &Json) -> Result<()> {
    for key in ["roles", "submission", "stress"] {
        member(v, key)?;
    }
    let stress = member(v, "command")?.as_str() == Some("stress");
    super::roles::read_roles(member(v, "roles")?, stress)?;
    for execution in member(v, "executions")?
        .as_array()
        .ok_or(Error("expected executions"))?
    {
        super::run::stage_role(
            super::read::read_stage(member(execution, "stage")?)?,
            &super::read::read_execution_role(member(execution, "role")?)?,
        )?;
    }
    for warning in member(v, "warnings")?
        .as_array()
        .ok_or(Error("expected warnings"))?
    {
        if matches!(
            warning.get("kind").and_then(Json::as_str),
            Some("judge_scratch" | "testlib_header")
        ) {
            super::roles::read_judge_warning(warning)?;
        }
    }
    if let Some(fingerprints) = v.get("fingerprints").filter(|v| !v.is_null()) {
        for key in ["source", "tests", "judge"] {
            let value = member(fingerprints, key)?;
            if key != "judge" || !value.is_null() {
                super::roles::read_hash(value)?;
            }
        }
    }
    let result = member(v, "result")?;
    let score = optional(member(result, "score")?, read_aggregate_score)?;
    let mut scoring = None;
    if let Some(target) = v.get("target").filter(|v| !v.is_null()) {
        let check = member(target, "check")?;
        if !check.is_null() {
            scoring = Some(read_check_config(check)?.scoring);
        }
        member(target, "io")?;
    }
    // Without a points `scoring` every score is `null` (report §2.13).
    let pass_fail = scoring == Some(r::Scoring::PassFail);
    if pass_fail && score.is_some() {
        return Err(Error("pass_fail report has an aggregate score"));
    }
    for test in member(v, "tests")?
        .as_array()
        .ok_or(Error("expected tests"))?
    {
        if optional(member(test, "score")?, parsed::<r::Score>)?.is_some() && pass_fail {
            return Err(Error("pass_fail test has a score"));
        }
        optional(member(test, "validation")?, read_validation)?;
        let judges = member(test, "judges")?
            .as_array()
            .ok_or(Error("expected judges"))?;
        if judges.len() > 2 {
            return Err(Error("too many judges"));
        }
        for judge in judges {
            read_judge(judge)?;
        }
        member(test, "session")?;
        member(test, "output_file")?;
        if let Some(check) = test.get("check").filter(|v| {
            matches!(
                v.get("kind").and_then(Json::as_str),
                Some("rejected" | "scored" | "judge_failed")
            ) || (v.get("kind").and_then(Json::as_str) == Some("unchecked")
                && v.get("reason").and_then(Json::as_str) != Some("no_answer"))
        }) {
            read_judged_check(check)?;
        }
    }
    Ok(())
}
