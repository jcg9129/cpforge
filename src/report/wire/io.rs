//! Ordered file-I/O members; these have no filesystem side effects.
use super::value::*;
use crate::report::{CollectionFailure, FileIo, OutputFile};

pub(super) fn test(v: &crate::jobs::engine::TestReport) -> Result<()> {
    use crate::report::{CheckKind, RunClass};
    let large = matches!(v.output_file, Some(OutputFile::TooLarge { .. }));
    match &v.class {
        Some(RunClass::OutputBudget { streams }) if streams.output_file != large => {
            return Err(Error("output-file budget stream does not match collection"));
        }
        Some(RunClass::ExitedZero | RunClass::NonzeroExit { .. } | RunClass::Signal { .. })
            if large =>
        {
            return Err(Error("oversized output did not raise the test class"));
        }
        _ => {}
    }
    if let Some(crate::report::CheckOutcome {
        kind: CheckKind::OutputRejected { reason },
        ..
    }) = &v.check
    {
        if v.output_file
            .as_ref()
            .and_then(OutputFile::rejection_reason)
            != Some(*reason)
        {
            return Err(Error("output rejection does not match collection"));
        }
    }
    Ok(())
}

pub(super) fn config(v: &FileIo) -> Result<Value> {
    if v.input.is_none() && v.output.is_none() {
        return Err(Error("empty file-I/O configuration"));
    }
    if let (Some(input), Some(output)) = (&v.input, &v.output) {
        if crate::names::collision_key(input.as_str())
            == crate::names::collision_key(output.as_str())
        {
            return Err(Error("file-I/O names collide"));
        }
    }
    Ok(obj([
        ("input", opt(v.input.as_ref(), |v| s(v.as_str()))),
        ("output", opt(v.output.as_ref(), |v| s(v.as_str()))),
        ("stdin", s(v.stdin_token())),
    ]))
}

pub(super) fn output_file(v: &OutputFile) -> Result<Value> {
    let mut fields = vec![("kind", s(v.wire_token()))];
    match v {
        OutputFile::Collected {
            bytes,
            sha256,
            path,
        } => {
            let run = path
                .strip_suffix("/out/output")
                .ok_or(Error("invalid output-file path"))?;
            super::schema::run_id(run)?;
            fields.extend([
                ("bytes", u(*bytes)),
                ("sha256", hex(sha256)),
                ("path", s(path)),
            ]);
        }
        OutputFile::NotRegular { file_type } => fields.push(("type", s(file_type.wire_token()))),
        OutputFile::Unreadable { os_error } => fields.push(("os_error", i(*os_error))),
        OutputFile::TooLarge { bytes, budget } => {
            if bytes <= budget {
                return Err(Error("output-file size does not exceed budget"));
            }
            fields.extend([("bytes", u(*bytes)), ("budget", u(*budget))]);
        }
        OutputFile::NotCollected { reason, os_error } => {
            if *reason != CollectionFailure::Io && os_error.is_some() {
                return Err(Error("non-I/O collection failure has OS error"));
            }
            fields.extend([
                ("reason", s(reason.wire_token())),
                ("os_error", opt(*os_error, i)),
            ]);
        }
        _ => {}
    }
    Ok(Value::Object(fields))
}
