//! The schema-1 admission sequence record and its §9 encodings.

use std::fmt;
use std::path::Path;

use toml_edit::DocumentMut;

use super::{Record, classify_tool_record, read_tool_record};

/// A §9 epoch encoding: exactly 32 lowercase ASCII hex digits.
/// Validation is shared by admission sequence and job metadata decoders.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Epoch(String);

impl Epoch {
    pub fn parse(value: &str) -> Result<Self, &'static str> {
        if value.len() == 32
            && value
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            Ok(Self(value.to_owned()))
        } else {
            Err("epoch must be exactly 32 lowercase hex digits")
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for Epoch {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Decode the §9 canonical decimal-string encoding without signed TOML integers.
pub(crate) fn parse_decimal_u64(value: &str) -> Option<u64> {
    if !value.is_empty()
        && value.bytes().all(|byte| byte.is_ascii_digit())
        && (value == "0" || !value.starts_with('0'))
    {
        value.parse().ok()
    } else {
        None
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct JobSequence {
    pub epoch: String,
    pub next: u64,
}

/// Classify an admission sequence without interpreting newer-schema fields.
pub fn classify_job_sequence(bytes: &[u8]) -> Record<JobSequence, &'static str> {
    classify_tool_record(bytes, 1, validate_fields)
}

/// Read a sequence from a path already checked through the workspace guard.
pub fn read_job_sequence(path: impl AsRef<Path>) -> Record<JobSequence, &'static str> {
    read_tool_record(path, 1, validate_fields)
}

// Field decoding is private so callers always go through schema classification.
fn validate_fields(document: &DocumentMut) -> Result<JobSequence, &'static str> {
    let epoch = document
        .get("epoch")
        .and_then(|item| item.as_str())
        .ok_or("epoch must be exactly 32 lowercase hex digits")?;
    let epoch = Epoch::parse(epoch)?;
    let next = document
        .get("next")
        .and_then(|item| item.as_str())
        .and_then(parse_decimal_u64)
        .ok_or("next must be a canonical decimal string no greater than u64::MAX")?;
    Ok(JobSequence {
        epoch: epoch.0,
        next,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::MalformedRecord;

    const EPOCH: &str = "0123456789abcdef0123456789abcdef";

    #[test]
    fn every_contract_decimal_encoding_is_classified() {
        for next in [
            "0",
            "9223372036854775807",
            "9223372036854775808",
            "18446744073709551614",
            "18446744073709551615",
        ] {
            let text = format!("schema = 1\nepoch = '{EPOCH}'\nnext = '{next}'\n");
            assert!(
                matches!(classify_job_sequence(text.as_bytes()),
                Record::Valid(record) if record.epoch == EPOCH && record.next == next.parse::<u64>().unwrap()),
                "{next}"
            );
        }
        for next in [
            "'18446744073709551616'",
            "'-1'",
            "'01'",
            "'+1'",
            "5",
            "''",
            "'00'",
            "' 1'",
            "'1 '",
            "'١'",
            "'１'",
            "'1_000'",
            "true",
            "1.0",
            "[]",
        ] {
            let text = format!("schema = 1\nepoch = '{EPOCH}'\nnext = {next}\n");
            assert!(
                matches!(
                    classify_job_sequence(text.as_bytes()),
                    Record::Malformed(MalformedRecord::Fields(_))
                ),
                "{next}"
            );
        }
    }

    #[test]
    fn epochs_require_exactly_32_lowercase_hex_digits() {
        for epoch in [
            "",
            "0123",
            "0123456789abcdef0123456789abcdef0",
            "0123456789abcdef0123456789abcdeF",
            "g123456789abcdef0123456789abcdef",
            "0123456789abcdef0123456789abcdeｆ",
        ] {
            let text = format!("schema = 1\nepoch = '{epoch}'\nnext = '0'\n");
            assert!(
                matches!(
                    classify_job_sequence(text.as_bytes()),
                    Record::Malformed(MalformedRecord::Fields(_))
                ),
                "{epoch}"
            );
        }
        for fields in [
            "next = '0'",
            "epoch = 5\nnext = '0'",
            "epoch = true\nnext = '0'",
            "epoch = []\nnext = '0'",
        ] {
            let text = format!("schema = 1\n{fields}\n");
            assert!(matches!(
                classify_job_sequence(text.as_bytes()),
                Record::Malformed(MalformedRecord::Fields(_))
            ));
        }
        let text = format!("schema = 1\nepoch = '{EPOCH}'\n");
        assert!(matches!(
            classify_job_sequence(text.as_bytes()),
            Record::Malformed(MalformedRecord::Fields(_))
        ));
    }

    #[test]
    fn schema_is_required_and_checked_before_fields() {
        assert!(matches!(
            classify_job_sequence(b"epoch = 'bad'\nnext = 5"),
            Record::Malformed(MalformedRecord::SchemaMissing)
        ));
        for schema in ["'1'", "true", "1.0", "[]"] {
            let text = format!("schema = {schema}");
            assert!(matches!(
                classify_job_sequence(text.as_bytes()),
                Record::Malformed(MalformedRecord::SchemaMalformed)
            ));
        }
        for schema in [0, -1] {
            let text = format!("schema = {schema}");
            assert!(matches!(
                classify_job_sequence(text.as_bytes()),
                Record::Malformed(MalformedRecord::SchemaTooOld { .. })
            ));
        }
        assert!(matches!(
            classify_job_sequence(b"schema = 99\nnext = 5"),
            Record::TooNew {
                schema: 99,
                supported: 1
            }
        ));
        assert!(matches!(
            classify_job_sequence(b"schema = 99\nbad = ["),
            Record::Malformed(MalformedRecord::Toml(_))
        ));
        assert!(matches!(
            classify_job_sequence(b"\xff"),
            Record::Malformed(MalformedRecord::Encoding(_))
        ));
    }
}
