//! Persisted reports need only the version prefix. Conformance readers also
//! detect duplicate members and validate the supported file-I/O extensions.
pub use super::judging::{
    read_aggregate_score, read_check_config, read_judge, read_judge_result, read_judged_check,
    read_validation,
};
use super::value::{Error, Result};
use serde::de::{self, Deserialize, Deserializer, MapAccess, SeqAccess, Visitor};
use std::collections::BTreeSet;
use std::fmt;

#[derive(Debug)]
struct Unique(serde_json::Value);
impl<'de> Deserialize<'de> for Unique {
    fn deserialize<D: Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = Unique;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("JSON with unique members and integer numbers")
            }
            fn visit_unit<E: de::Error>(self) -> std::result::Result<Unique, E> {
                Ok(Unique(serde_json::Value::Null))
            }
            fn visit_bool<E: de::Error>(self, v: bool) -> std::result::Result<Unique, E> {
                Ok(Unique(v.into()))
            }
            fn visit_i64<E: de::Error>(self, v: i64) -> std::result::Result<Unique, E> {
                Ok(Unique(v.into()))
            }
            fn visit_u64<E: de::Error>(self, v: u64) -> std::result::Result<Unique, E> {
                Ok(Unique(v.into()))
            }
            fn visit_f64<E: de::Error>(self, _: f64) -> std::result::Result<Unique, E> {
                Err(E::custom("fraction/exponent number"))
            }
            fn visit_str<E: de::Error>(self, v: &str) -> std::result::Result<Unique, E> {
                Ok(Unique(v.into()))
            }
            fn visit_seq<A: SeqAccess<'de>>(
                self,
                mut a: A,
            ) -> std::result::Result<Unique, A::Error> {
                let mut v = Vec::new();
                while let Some(Unique(item)) = a.next_element()? {
                    v.push(item);
                }
                Ok(Unique(v.into()))
            }
            fn visit_map<A: MapAccess<'de>>(
                self,
                mut a: A,
            ) -> std::result::Result<Unique, A::Error> {
                let mut keys = BTreeSet::new();
                let mut v = serde_json::Map::new();
                while let Some(key) = a.next_key::<String>()? {
                    if !keys.insert(key.clone()) {
                        return Err(de::Error::custom("duplicate member"));
                    }
                    let Unique(item) = a.next_value()?;
                    v.insert(key, item);
                }
                Ok(Unique(v.into()))
            }
        }
        d.deserialize_any(V)
    }
}
pub fn read_json(bytes: &[u8]) -> Result<serde_json::Value> {
    serde_json::from_slice::<Unique>(bytes)
        .map(|v| v.0)
        .map_err(|_| Error("invalid JSON, duplicate member or non-integer number"))
}
/// Validate the schema-1 envelope and supported job file-I/O extensions.
/// Other document-specific members still require their own readers.
pub fn read_document_kind(bytes: &[u8]) -> Result<crate::report::DocumentKind> {
    let value = read_json(bytes)?;
    if value.get("schema").and_then(serde_json::Value::as_u64) != Some(1)
        || !matches!(
            value.get("platform").and_then(serde_json::Value::as_str),
            Some("unix" | "windows")
        )
    {
        return Err(Error("expected schema-1 document envelope"));
    }
    let kind = serde_json::from_value(
        value
            .get("document")
            .cloned()
            .ok_or(Error("missing document kind"))?,
    )
    .map_err(|_| Error("unknown document kind"))?;
    if kind == crate::report::DocumentKind::JobReport {
        validate_file_io_members(&value)?;
        super::judging::validate_members(&value)?;
    }
    Ok(kind)
}

fn member<'a>(value: &'a serde_json::Value, key: &str) -> Result<&'a serde_json::Value> {
    value.get(key).ok_or(Error("missing required member"))
}
fn string(value: &serde_json::Value) -> Result<&str> {
    value.as_str().ok_or(Error("expected string"))
}
fn enumeration<T: serde::de::DeserializeOwned>(value: &serde_json::Value) -> Result<T> {
    serde_json::from_value(value.clone()).map_err(|_| Error("unknown enum value"))
}
fn os_error(value: &serde_json::Value) -> Result<i32> {
    value
        .as_i64()
        .and_then(|v| v.try_into().ok())
        .ok_or(Error("invalid i32"))
}
pub fn read_file_io(value: &serde_json::Value) -> Result<crate::report::FileIo> {
    let name = |key| -> Result<Option<crate::names::PortableComponent>> {
        let v = member(value, key)?;
        if v.is_null() {
            return Ok(None);
        }
        crate::names::PortableComponent::new(string(v)?)
            .map(Some)
            .map_err(|_| Error("invalid file-I/O name"))
    };
    let io = crate::report::FileIo {
        input: name("input")?,
        output: name("output")?,
    };
    super::io::config(&io)?;
    if string(member(value, "stdin")?)? != io.stdin_token() {
        return Err(Error("file-I/O stdin does not match input"));
    }
    Ok(io)
}
pub fn read_output_file(value: &serde_json::Value) -> Result<crate::report::OutputFile> {
    use crate::report::OutputFile as O;
    let output = match string(member(value, "kind")?)? {
        "collected" => {
            let hex = string(member(value, "sha256")?)?;
            if hex.len() != 64
                || !hex
                    .bytes()
                    .all(|v| v.is_ascii_digit() || (b'a'..=b'f').contains(&v))
            {
                return Err(Error("invalid hex64"));
            }
            let mut sha256 = [0; 32];
            for (i, byte) in sha256.iter_mut().enumerate() {
                *byte = u8::from_str_radix(&hex[2 * i..2 * i + 2], 16)
                    .map_err(|_| Error("invalid hex64"))?;
            }
            O::Collected {
                bytes: read_u64(member(value, "bytes")?)?,
                sha256,
                path: string(member(value, "path")?)?.to_owned(),
            }
        }
        "missing" => O::Missing,
        "misnamed" => O::Misnamed,
        "too_many_entries" => O::TooManyEntries,
        "not_regular" => O::NotRegular {
            file_type: enumeration(member(value, "type")?)?,
        },
        "unreadable" => O::Unreadable {
            os_error: os_error(member(value, "os_error")?)?,
        },
        "too_large" => O::TooLarge {
            bytes: read_u64(member(value, "bytes")?)?,
            budget: read_u64(member(value, "budget")?)?,
        },
        "not_collected" => {
            let error = member(value, "os_error")?;
            O::NotCollected {
                reason: enumeration(member(value, "reason")?)?,
                os_error: if error.is_null() {
                    None
                } else {
                    Some(os_error(error)?)
                },
            }
        }
        "not_attempted" => O::NotAttempted,
        _ => return Err(Error("unknown output-file outcome")),
    };
    super::io::output_file(&output)?;
    Ok(output)
}
pub fn read_output_rejected(value: &serde_json::Value) -> Result<crate::report::CheckKind> {
    if string(member(value, "kind")?)? != "output_rejected" {
        return Err(Error("expected output_rejected"));
    }
    Ok(crate::report::CheckKind::OutputRejected {
        reason: enumeration(member(value, "reason")?)?,
    })
}
pub fn read_io_scratch(value: &serde_json::Value) -> Result<super::Warning> {
    if string(member(value, "kind")?)? != "io_scratch" {
        return Err(Error("expected io_scratch"));
    }
    let run = string(member(value, "run")?)?;
    super::schema::run_id(run)?;
    let message = string(member(value, "message")?)?;
    if super::value::encode(&super::value::s(message)).len() > 260 {
        return Err(Error("warning message exceeds bound"));
    }
    Ok(super::Warning {
        kind: super::WarningKind::IoScratch {
            run: run.to_owned(),
            detail: enumeration(member(value, "detail")?)?,
        },
        message: message.to_owned(),
    })
}
pub fn read_budget_streams(value: &serde_json::Value) -> Result<crate::report::BudgetStreams> {
    use crate::report::BudgetStreams;
    if string(member(value, "kind")?)? != "output_budget" {
        return Err(Error("expected output_budget"));
    }
    let array = member(value, "streams")?
        .as_array()
        .ok_or(Error("expected streams array"))?;
    let mut streams = BudgetStreams {
        stdout: false,
        stderr: false,
        output_file: false,
    };
    for value in array {
        match string(value)? {
            "stdout" => streams.stdout = true,
            "stderr" => streams.stderr = true,
            "output_file" => streams.output_file = true,
            _ => return Err(Error("unknown budget stream")),
        }
    }
    let expected: Vec<_> = streams
        .streams()
        .map(|v| serde_json::Value::from(v.wire_token()))
        .collect();
    if *array != expected
        || streams.primary().map(|v| v.wire_token()) != Some(string(member(value, "stream")?)?)
    {
        return Err(Error("invalid budget stream ordering or primary"));
    }
    Ok(streams)
}

/// Validate all file-I/O additions while retaining the legacy report profile.
/// Unknown members remain ignorable, as required by §3.5.
fn validate_file_io_members(value: &serde_json::Value) -> Result<()> {
    let io = value
        .get("target")
        .and_then(|v| v.get("io"))
        .filter(|v| !v.is_null())
        .map(read_file_io)
        .transpose()?;
    if io.is_some()
        && (value.get("command").and_then(serde_json::Value::as_str) != Some("test")
            || value
                .get("problem")
                .and_then(|v| v.get("kind"))
                .and_then(serde_json::Value::as_str)
                == Some("interactive"))
    {
        return Err(Error("file I/O is only applied to test jobs"));
    }
    if let Some(tests) = value.get("tests").and_then(serde_json::Value::as_array) {
        for test in tests {
            let output = test
                .get("output_file")
                .filter(|v| !v.is_null())
                .map(read_output_file)
                .transpose()?;
            if output.is_some() != io.as_ref().is_some_and(|io| io.output.is_some()) {
                return Err(Error("output-file evidence does not match target I/O"));
            }
            if io.is_some() {
                member(test, "output_file")?;
            }
            if let Some(check) = test.get("check").filter(|v| {
                v.get("kind").and_then(serde_json::Value::as_str) == Some("output_rejected")
            }) {
                if let crate::report::CheckKind::OutputRejected { reason } =
                    read_output_rejected(check)?
                {
                    if output
                        .as_ref()
                        .and_then(crate::report::OutputFile::rejection_reason)
                        != Some(reason)
                    {
                        return Err(Error("output rejection does not match collection"));
                    }
                }
            }
            if matches!(output, Some(crate::report::OutputFile::TooLarge { .. }))
                && matches!(
                    test.get("class")
                        .and_then(|v| v.get("kind"))
                        .and_then(serde_json::Value::as_str),
                    Some("exited_zero" | "nonzero_exit" | "signal")
                )
            {
                return Err(Error("oversized output did not raise the test class"));
            }
            if let Some(class) = test.get("class").filter(|v| {
                v.get("kind").and_then(serde_json::Value::as_str) == Some("output_budget")
            }) {
                let streams = read_budget_streams(class)?;
                if streams.output_file
                    != matches!(output, Some(crate::report::OutputFile::TooLarge { .. }))
                {
                    return Err(Error("output-file budget stream does not match collection"));
                }
            }
        }
    }
    if let Some(executions) = value
        .get("executions")
        .and_then(serde_json::Value::as_array)
    {
        for entry in executions {
            for class in [
                entry.get("class"),
                entry.get("summary").and_then(|v| v.get("class")),
            ]
            .into_iter()
            .flatten()
            {
                if class.get("kind").and_then(serde_json::Value::as_str) == Some("output_budget")
                    && read_budget_streams(class)?.output_file
                {
                    return Err(Error("output_file stream is only valid in a test class"));
                }
            }
        }
    }
    if let Some(warnings) = value.get("warnings").and_then(serde_json::Value::as_array) {
        for warning in warnings {
            if warning.get("kind").and_then(serde_json::Value::as_str) == Some("io_scratch") {
                read_io_scratch(warning)?;
            }
        }
    }
    Ok(())
}

pub fn read_u64(value: &serde_json::Value) -> Result<u64> {
    let s = value
        .as_str()
        .ok_or(Error("u64 must be a decimal string"))?;
    unsigned(s)?;
    s.parse().map_err(|_| Error("u64 out of range"))
}
pub fn read_i64(value: &serde_json::Value) -> Result<i64> {
    let s = value
        .as_str()
        .ok_or(Error("i64 must be a decimal string"))?;
    let digits = s.strip_prefix('-').unwrap_or(s);
    unsigned(digits)?;
    if s == "-0" {
        return Err(Error("negative zero"));
    }
    s.parse().map_err(|_| Error("i64 out of range"))
}
fn unsigned(s: &str) -> Result<()> {
    if s.is_empty() || !s.bytes().all(|b| b.is_ascii_digit()) || (s.len() > 1 && s.starts_with('0'))
    {
        return Err(Error("non-canonical decimal string"));
    }
    Ok(())
}
pub fn read_u32(value: &serde_json::Value) -> Result<u32> {
    value
        .as_u64()
        .and_then(|v| v.try_into().ok())
        .ok_or(Error("invalid u32"))
}

/// Classifies the canonical report prefix without reading beyond 32 bytes.
/// This identifies a version only; it does not validate a complete report.
/// Unknown/newer versions must be protected by the pruning caller.
pub fn report_schema_prefix(bytes: &[u8]) -> Result<u32> {
    let bytes = &bytes[..bytes.len().min(32)];
    let rest = bytes
        .strip_prefix(b"{\"schema\":")
        .ok_or(Error("invalid report prefix"))?;
    let end = rest
        .iter()
        .position(|b| !b.is_ascii_digit())
        .ok_or(Error("incomplete schema prefix"))?;
    let digits = std::str::from_utf8(&rest[..end]).map_err(|_| Error("invalid schema"))?;
    unsigned(digits)?;
    let schema = digits.parse().map_err(|_| Error("schema out of range"))?;
    const EXPECTED: &[u8] = b",\"document\":\"job_report\"";
    let suffix = &rest[end..];
    let complete = suffix.starts_with(EXPECTED);
    // Only a full 32-byte window may end inside the document name: a long
    // future version is identified before the name fits. Shorter input is a
    // truncated file, never a report.
    let cut = bytes.len() == 32 && EXPECTED.starts_with(suffix);
    if !(complete || cut) {
        return Err(Error("invalid report document prefix"));
    }
    Ok(schema)
}
