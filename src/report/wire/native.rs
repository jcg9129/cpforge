use super::value::*;
use sha2::{Digest, Sha256};
use std::ffi::{OsStr, OsString};

/// Lossless Unix bytes or Windows WTF-8, with paired surrogates encoded normally.
pub fn native_bytes(value: &OsStr) -> Vec<u8> {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        value.as_bytes().to_vec()
    }
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        let mut bytes = Vec::new();
        for ch in std::char::decode_utf16(value.encode_wide()) {
            match ch {
                Ok(ch) => {
                    let mut buf = [0; 4];
                    bytes.extend_from_slice(ch.encode_utf8(&mut buf).as_bytes());
                }
                Err(err) => {
                    let c = u32::from(err.unpaired_surrogate());
                    bytes.extend_from_slice(&[
                        (0xe0 | (c >> 12)) as u8,
                        (0x80 | ((c >> 6) & 63)) as u8,
                        (0x80 | (c & 63)) as u8,
                    ]);
                }
            }
        }
        bytes
    }
}
pub fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let a = chunk[0];
        let b = chunk.get(1).copied().unwrap_or(0);
        let c = chunk.get(2).copied().unwrap_or(0);
        out.push(char::from(ALPHABET[usize::from(a >> 2)]));
        out.push(char::from(ALPHABET[usize::from(((a & 3) << 4) | (b >> 4))]));
        out.push(if chunk.len() > 1 {
            char::from(ALPHABET[usize::from(((b & 15) << 2) | (c >> 6))])
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            char::from(ALPHABET[usize::from(c & 63)])
        } else {
            '='
        });
    }
    out
}
pub fn decode_base64(value: &str) -> Result<Vec<u8>> {
    fn digit(b: u8) -> Result<u8> {
        Ok(match b {
            b'A'..=b'Z' => b - b'A',
            b'a'..=b'z' => b - b'a' + 26,
            b'0'..=b'9' => b - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            _ => return Err(Error("invalid base64")),
        })
    }
    if value.len() % 4 != 0 {
        return Err(Error("invalid base64 padding"));
    }
    let mut out = Vec::new();
    for chunk in value.as_bytes().chunks_exact(4) {
        let a = digit(chunk[0])?;
        let b = digit(chunk[1])?;
        let c = if chunk[2] == b'=' {
            0
        } else {
            digit(chunk[2])?
        };
        let d = if chunk[3] == b'=' {
            0
        } else {
            digit(chunk[3])?
        };
        out.push((a << 2) | (b >> 4));
        if chunk[2] != b'=' {
            out.push((b << 4) | (c >> 2));
        }
        if chunk[3] != b'=' {
            out.push((c << 6) | d);
        }
    }
    if base64(&out) != value {
        return Err(Error("non-canonical base64"));
    }
    Ok(out)
}
pub(super) fn eligible(bytes: &[u8]) -> Option<&str> {
    std::str::from_utf8(bytes)
        .ok()
        .filter(|s| !s.chars().any(|c| c <= '\u{1f}' || c == '\u{7f}'))
}
pub(super) fn ns(bytes: &[u8]) -> Value {
    match eligible(bytes) {
        Some(value) => obj([("utf8", s(value))]),
        None => raw(bytes),
    }
}
pub(super) fn raw(bytes: &[u8]) -> Value {
    obj([("bytes_b64", s(base64(bytes)))])
}
pub(super) fn nsum(bytes: &[u8], cap: usize) -> Value {
    let mut end = bytes.len().min(cap);
    if let Some(value) = eligible(bytes) {
        while !value.is_char_boundary(end) {
            end -= 1;
        }
    }
    obj([
        ("head", ns(&bytes[..end])),
        ("len", u(bytes.len() as u64)),
        ("truncated", b(end < bytes.len())),
        ("sha256", hex(&Sha256::digest(bytes))),
    ])
}
pub fn encode_native(value: &OsStr) -> Vec<u8> {
    encode(&ns(&native_bytes(value)))
}
pub fn encode_native_summary(value: &OsStr, cap: usize) -> Vec<u8> {
    encode(&nsum(&native_bytes(value), cap))
}
pub fn encode_bytes(value: &[u8]) -> Vec<u8> {
    encode(&raw(value))
}

/// Decode canonical native JSON using the current platform's native string type.
pub fn decode_native(json: &[u8]) -> Result<OsString> {
    let value = super::read::read_json(json)?;
    let object = value
        .as_object()
        .ok_or(Error("native value must be an object"))?;
    let utf8 = object.get("utf8");
    let b64 = object.get("bytes_b64");
    let bytes = match (utf8, b64) {
        (Some(v), None) => {
            let v = v.as_str().ok_or(Error("utf8 must be a string"))?;
            if eligible(v.as_bytes()).is_none() {
                return Err(Error("ineligible utf8"));
            }
            v.as_bytes().to_vec()
        }
        (None, Some(v)) => {
            let bytes = decode_base64(v.as_str().ok_or(Error("base64 must be a string"))?)?;
            if eligible(&bytes).is_some() {
                return Err(Error("non-canonical native base64"));
            }
            bytes
        }
        _ => return Err(Error("native value needs exactly one representation")),
    };
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStringExt;
        Ok(OsString::from_vec(bytes))
    }
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStringExt;
        // Decode WTF-8 by admitting isolated surrogate triples, then enforce
        // canonical pairing by re-encoding the resulting native value.
        let mut units = Vec::new();
        let mut start = 0;
        let mut j = 0;
        while j < bytes.len() {
            if j + 2 < bytes.len()
                && bytes[j] == 0xed
                && (0xa0..=0xbf).contains(&bytes[j + 1])
                && (0x80..=0xbf).contains(&bytes[j + 2])
            {
                let text =
                    std::str::from_utf8(&bytes[start..j]).map_err(|_| Error("invalid WTF-8"))?;
                units.extend(text.encode_utf16());
                units.push(
                    (u16::from(bytes[j] & 15) << 12)
                        | (u16::from(bytes[j + 1] & 63) << 6)
                        | u16::from(bytes[j + 2] & 63),
                );
                j += 3;
                start = j;
            } else {
                j += 1;
            }
        }
        units.extend(
            std::str::from_utf8(&bytes[start..])
                .map_err(|_| Error("invalid WTF-8"))?
                .encode_utf16(),
        );
        let value = OsString::from_wide(&units);
        if native_bytes(&value) != bytes {
            return Err(Error("non-canonical WTF-8"));
        }
        Ok(value)
    }
}
