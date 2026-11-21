use std::fmt;

/// An encoding error is missing evidence, an invalid field, or an exceeded bound.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Error(pub &'static str);
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.0)
    }
}
impl std::error::Error for Error {}
pub type Result<T> = std::result::Result<T, Error>;

// Ordered objects are intentional: serde_json::Map's ordering is not the schema.
#[derive(Clone, Debug)]
pub(super) enum Value {
    Null,
    Verbatim(Vec<u8>),
    Bool(bool),
    Number(i64),
    String(std::string::String),
    Array(Vec<Value>),
    Object(Vec<(&'static str, Value)>),
}
use Value::*;
pub(super) fn s(value: impl Into<std::string::String>) -> Value {
    String(value.into())
}
pub(super) fn u(value: u64) -> Value {
    s(value.to_string())
}
pub(super) fn n(value: u32) -> Value {
    Number(i64::from(value))
}
pub(super) fn i(value: i32) -> Value {
    Number(i64::from(value))
}
pub(super) fn b(value: bool) -> Value {
    Bool(value)
}
pub(super) fn obj<const N: usize>(members: [(&'static str, Value); N]) -> Value {
    Object(members.into())
}
pub(super) fn kind(value: &str) -> Value {
    obj([("kind", s(value))])
}
pub(super) fn opt<T>(value: Option<T>, f: impl FnOnce(T) -> Value) -> Value {
    value.map_or(Null, f)
}
pub(super) fn array<T>(values: impl IntoIterator<Item = T>, f: impl FnMut(T) -> Value) -> Value {
    Array(values.into_iter().map(f).collect())
}
pub(super) fn hex(bytes: &[u8]) -> Value {
    const HEX: &[u8] = b"0123456789abcdef";
    let mut value = std::string::String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        value.push(char::from(HEX[usize::from(byte >> 4)]));
        value.push(char::from(HEX[usize::from(byte & 15)]));
    }
    s(value)
}
pub(super) fn token(value: &str) -> Result<Value> {
    if value.is_empty()
        || value.len() > 32
        || !value
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
    {
        return Err(Error("invalid token"));
    }
    Ok(s(value))
}
pub(super) fn code(value: i64) -> Result<Value> {
    if !(-2_147_483_648..=4_294_967_295).contains(&value) {
        return Err(Error("exit code out of range"));
    }
    Ok(Number(value))
}
pub(super) fn dur(value: std::time::Duration) -> Result<Value> {
    Ok(u(
        u64::try_from(value.as_micros()).map_err(|_| Error("duration out of range"))?
    ))
}
pub(super) fn ident(value: &str) -> Result<Value> {
    crate::names::Identifier::new(value.to_owned()).map_err(|_| Error("invalid identifier"))?;
    Ok(s(value))
}
pub(super) fn rel(value: &std::path::Path, max: usize) -> Result<Value> {
    use std::path::Component;
    let mut parts = Vec::new();
    for part in value.components() {
        match part {
            Component::Normal(part) => {
                parts.push(part.to_str().ok_or(Error("non-Unicode relative path"))?)
            }
            _ => return Err(Error("invalid relative path")),
        }
    }
    let value = parts.join("/");
    if value.is_empty() || encode(&s(&value)).len() > max + 2 {
        return Err(Error("relative path exceeds bound"));
    }
    Ok(s(value))
}
fn quoted(value: &str, out: &mut Vec<u8>) {
    out.push(b'"');
    for ch in value.chars() {
        match ch {
            '"' => out.extend_from_slice(b"\\\""),
            '\\' => out.extend_from_slice(b"\\\\"),
            '\n' => out.extend_from_slice(b"\\n"),
            '\r' => out.extend_from_slice(b"\\r"),
            '\t' => out.extend_from_slice(b"\\t"),
            '\0'..='\u{1f}' => out.extend_from_slice(format!("\\u00{:02x}", ch as u32).as_bytes()),
            _ => {
                let mut buf = [0; 4];
                out.extend_from_slice(ch.encode_utf8(&mut buf).as_bytes());
            }
        }
    }
    out.push(b'"');
}
/// Informative text is capped by its *serialized* size, including ellipsis.
pub(super) fn text(value: &str) -> Value {
    if encode(&s(value)).len() <= 260 {
        return s(value);
    }
    let mut head = std::string::String::new();
    for ch in value.chars() {
        head.push(ch);
        if encode(&s(format!("{head}…"))).len() > 260 {
            head.pop();
            break;
        }
    }
    s(format!("{head}…"))
}
pub(super) fn encode(value: &Value) -> Vec<u8> {
    fn write(value: &Value, out: &mut Vec<u8>) {
        match value {
            Null => out.extend_from_slice(b"null"),
            Verbatim(v) => out.extend_from_slice(v),
            Bool(v) => out.extend_from_slice(if *v { b"true" } else { b"false" }),
            Number(v) => out.extend_from_slice(v.to_string().as_bytes()),
            String(v) => quoted(v, out),
            Array(v) => {
                out.push(b'[');
                for (j, v) in v.iter().enumerate() {
                    if j > 0 {
                        out.push(b',');
                    }
                    write(v, out);
                }
                out.push(b']');
            }
            Object(v) => {
                out.push(b'{');
                for (j, (k, v)) in v.iter().enumerate() {
                    if j > 0 {
                        out.push(b',');
                    }
                    quoted(k, out);
                    out.push(b':');
                    write(v, out);
                }
                out.push(b'}');
            }
        }
    }
    let mut out = Vec::new();
    write(value, &mut out);
    out
}
pub(super) fn document(
    name: &str,
    platform: &str,
    mut fields: Vec<(&'static str, Value)>,
) -> Result<Value> {
    if !matches!(platform, "unix" | "windows") {
        return Err(Error("invalid platform"));
    }
    let mut members = vec![
        ("schema", n(1)),
        ("document", token(name)?),
        ("platform", s(platform)),
    ];
    members.append(&mut fields);
    Ok(Object(members))
}
pub(super) fn bounded(value: &Value, bound: usize) -> Result<Vec<u8>> {
    let bytes = encode(value);
    if bytes.len() > bound {
        return Err(Error("serialized size exceeds contract bound"));
    }
    Ok(bytes)
}
pub(super) fn dropped(len: usize, cap: usize, previous: u32) -> Value {
    n(previous.saturating_add(len.saturating_sub(cap).min(u32::MAX as usize) as u32))
}
pub(super) fn checked_array<T>(
    values: &[T],
    cap: usize,
    f: impl FnMut(&T) -> Result<Value>,
) -> Result<Value> {
    if values.len() > cap {
        return Err(Error("domain list exceeds bound"));
    }
    Ok(Array(values.iter().map(f).collect::<Result<_>>()?))
}
