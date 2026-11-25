//! cpf reads only the version prefix from persisted reports. The full JSON
//! helper also provides duplicate detection for field/native conformance tools.
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
/// Validate the common schema-1 envelope only. Document-specific members must
/// still be checked by their reader; pending document stubs have no such reader.
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
    serde_json::from_value(
        value
            .get("document")
            .cloned()
            .ok_or(Error("missing document kind"))?,
    )
    .map_err(|_| Error("unknown document kind"))
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
