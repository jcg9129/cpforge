//! Schema-first classification of user-editable and tool-only TOML records (§9).

use std::fs::OpenOptions;
use std::io::{self, Read};
use std::path::Path;

use toml_edit::{DocumentMut, ImDocument};

/// Maximum bytes read or parsed for a small editable metadata record.
pub(crate) const RECORD_SIZE_LIMIT: usize = 1024 * 1024;

#[derive(Debug)]
pub enum Record<T, E> {
    Missing,
    Unreadable(io::Error),
    Malformed(MalformedRecord<E>),
    TooNew { schema: i64, supported: i64 },
    Valid(T),
}

#[derive(Debug)]
pub enum MalformedRecord<E> {
    TooLarge { limit: usize },
    Encoding(std::str::Utf8Error),
    Toml(toml_edit::TomlError),
    SchemaMissing,
    SchemaMalformed,
    SchemaTooOld { schema: i64, supported: i64 },
    Fields(E),
}

impl<E: std::fmt::Display> std::fmt::Display for MalformedRecord<E> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::TooLarge { limit } => write!(f, "record exceeds the {limit}-byte size limit"),
            Self::Encoding(error) => write!(f, "not UTF-8: {error}"),
            Self::Toml(error) => write!(f, "not TOML: {}", error.message()),
            Self::SchemaMissing => write!(f, "`schema` is missing"),
            Self::SchemaMalformed => write!(f, "`schema` is not an integer"),
            Self::SchemaTooOld { schema, supported } => {
                write!(f, "schema {schema} is older than supported {supported}")
            }
            Self::Fields(error) => write!(f, "{error}"),
        }
    }
}

/// Read bytes from a regular file (read-only paths may be links), then classify
/// them. The format validator runs only after the schema has been accepted.
pub fn read_user_record<T, E>(
    path: impl AsRef<Path>,
    supported: i64,
    validate: impl FnOnce(&DocumentMut) -> Result<T, E>,
) -> Record<T, E> {
    read_record(path.as_ref(), |bytes| {
        classify_user_record(bytes, supported, validate)
    })
}

/// Read and classify a tool-only record, requiring an explicit schema.
/// Check tool-owned paths through the workspace guard before calling this;
/// like other read-only record access, this reader may follow links.
pub fn read_tool_record<T, E>(
    path: impl AsRef<Path>,
    supported: i64,
    validate: impl FnOnce(&DocumentMut) -> Result<T, E>,
) -> Record<T, E> {
    read_record(path.as_ref(), |bytes| {
        classify_tool_record(bytes, supported, validate)
    })
}

fn read_record<T, E>(path: &Path, classify: impl FnOnce(&[u8]) -> Record<T, E>) -> Record<T, E> {
    match read_record_bytes(path) {
        RecordBytes::Missing => Record::Missing,
        RecordBytes::Unreadable(error) => Record::Unreadable(error),
        RecordBytes::Present(bytes) => classify(&bytes),
        RecordBytes::TooLarge => Record::Malformed(MalformedRecord::TooLarge {
            limit: RECORD_SIZE_LIMIT,
        }),
    }
}

/// Shared regular-file reader. Keeping the bytes lets span-aware consumers map
/// syntax-error offsets without parsing a diagnostic's rendered text.
#[derive(Debug)]
pub(crate) enum RecordBytes {
    Missing,
    Unreadable(io::Error),
    Present(Vec<u8>),
    TooLarge,
}

pub(crate) fn read_record_bytes(path: &Path) -> RecordBytes {
    let mut open = OpenOptions::new();
    open.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        open.custom_flags(libc::O_NONBLOCK | libc::O_CLOEXEC);
    }
    let file = match open.open(path) {
        Ok(file) => file,
        Err(error)
            if matches!(
                error.kind(),
                io::ErrorKind::NotFound | io::ErrorKind::NotADirectory
            ) =>
        {
            return RecordBytes::Missing;
        }
        Err(error) => return RecordBytes::Unreadable(error),
    };
    match file.metadata() {
        Ok(metadata) if metadata.is_file() => {
            if metadata.len() > RECORD_SIZE_LIMIT as u64 {
                return RecordBytes::TooLarge;
            }
        }
        Ok(_) => return RecordBytes::Unreadable(io::Error::other("record is not a regular file")),
        Err(error) => return RecordBytes::Unreadable(error),
    }
    let mut bytes = Vec::new();
    #[cfg(feature = "storage-test-hooks")]
    let file = {
        let mut file = file;
        let half = file.metadata().map_or(1, |metadata| {
            (metadata.len().min(RECORD_SIZE_LIMIT as u64 + 1) / 2).max(1)
        });
        if let Err(error) = file.by_ref().take(half).read_to_end(&mut bytes) {
            return RecordBytes::Unreadable(error);
        }
        crate::storage::crash::barrier("org.read_partial");
        file
    };
    let remaining = RECORD_SIZE_LIMIT as u64 + 1 - bytes.len() as u64;
    if let Err(error) = file.take(remaining).read_to_end(&mut bytes) {
        return RecordBytes::Unreadable(error);
    }
    if bytes.len() > RECORD_SIZE_LIMIT {
        return RecordBytes::TooLarge;
    }
    RecordBytes::Present(bytes)
}

/// Parse TOML, read `schema` alone (absent means 1), then decode/validate the
/// supported format. Newer schemas are classified without invoking `validate`.
pub fn classify_user_record<T, E>(
    bytes: &[u8],
    supported: i64,
    validate: impl FnOnce(&DocumentMut) -> Result<T, E>,
) -> Record<T, E> {
    classify_record(bytes, supported, false, |document| {
        validate(&document.into_mut())
    })
}

/// Schema-first classification retaining the original document and its spans.
pub fn classify_user_record_spanned<T, E>(
    bytes: &[u8],
    supported: i64,
    validate: impl FnOnce(&ImDocument<String>) -> Result<T, E>,
) -> Record<T, E> {
    classify_record(bytes, supported, false, |document| validate(&document))
}

/// Parse TOML and require `schema` before validating any tool-only fields.
/// Newer schemas are classified without invoking the format validator.
pub fn classify_tool_record<T, E>(
    bytes: &[u8],
    supported: i64,
    validate: impl FnOnce(&DocumentMut) -> Result<T, E>,
) -> Record<T, E> {
    classify_record(bytes, supported, true, |document| {
        validate(&document.into_mut())
    })
}

fn classify_record<T, E>(
    bytes: &[u8],
    supported: i64,
    schema_required: bool,
    validate: impl FnOnce(ImDocument<String>) -> Result<T, E>,
) -> Record<T, E> {
    if bytes.len() > RECORD_SIZE_LIMIT {
        return Record::Malformed(MalformedRecord::TooLarge {
            limit: RECORD_SIZE_LIMIT,
        });
    }
    let text = match std::str::from_utf8(bytes) {
        Ok(text) => text,
        Err(error) => return Record::Malformed(MalformedRecord::Encoding(error)),
    };
    let document = match ImDocument::parse(text.to_owned()) {
        Ok(document) => document,
        Err(error) => return Record::Malformed(MalformedRecord::Toml(error)),
    };
    let schema = match document.get("schema") {
        None if schema_required => return Record::Malformed(MalformedRecord::SchemaMissing),
        None => 1,
        Some(item) => match item.as_integer() {
            Some(schema) => schema,
            None => return Record::Malformed(MalformedRecord::SchemaMalformed),
        },
    };
    if schema > supported {
        return Record::TooNew { schema, supported };
    }
    if schema < supported {
        return Record::Malformed(MalformedRecord::SchemaTooOld { schema, supported });
    }
    match validate(document) {
        Ok(value) => Record::Valid(value),
        Err(error) => Record::Malformed(MalformedRecord::Fields(error)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn toy(document: &DocumentMut) -> Result<String, &'static str> {
        document
            .get("name")
            .and_then(|item| item.as_str())
            .map(str::to_owned)
            .ok_or("name must be a string")
    }

    #[test]
    fn spanned_classification_retains_original_offsets() {
        let bytes = b"# original\nname = 'toy'\n";
        let record = classify_user_record_spanned(bytes, 1, |document| {
            assert_eq!(document.raw().as_bytes(), bytes);
            Ok::<_, ()>(document.get("name").unwrap().span().unwrap())
        });
        let Record::Valid(span) = record else {
            panic!("expected valid record")
        };
        assert_eq!(&bytes[span], b"'toy'");
        assert!(matches!(
            classify_user_record_spanned(b"schema = 2", 1, |_| -> Result<(), ()> {
                panic!("newer schema must not be decoded")
            }),
            Record::TooNew {
                schema: 2,
                supported: 1
            }
        ));
    }

    #[test]
    fn supported_schema_decodes_only_after_schema_classification() {
        for text in ["name = 'toy'", "schema = 1\nname = 'toy'"] {
            assert!(matches!(classify_user_record(text.as_bytes(), 1, toy),
                Record::Valid(name) if name == "toy"));
        }
        assert!(matches!(
            classify_user_record(b"schema = 1", 1, toy),
            Record::Malformed(MalformedRecord::Fields(_))
        ));
        assert!(matches!(
            classify_user_record(b"schema = 1\nname = 4", 1, toy),
            Record::Malformed(MalformedRecord::Fields(_))
        ));
        assert!(matches!(
            classify_user_record(b"schema = 2\nname = []", 1, |_| -> Result<(), ()> {
                panic!("must not decode a newer schema")
            }),
            Record::TooNew { schema: 2, .. }
        ));
    }

    #[test]
    fn syntax_encoding_and_schema_errors_precede_field_validation() {
        let no_decode = |_: &DocumentMut| -> Result<(), ()> { panic!("must not decode") };
        for text in ["schema = '1'", "schema = 1.0", "schema = true", "[schema]"] {
            assert!(matches!(
                classify_user_record(text.as_bytes(), 1, no_decode),
                Record::Malformed(MalformedRecord::SchemaMalformed)
            ));
        }
        for text in ["schema = 0", "schema = -1", "name = 'old'"] {
            assert!(matches!(
                classify_user_record(text.as_bytes(), 2, no_decode),
                Record::Malformed(MalformedRecord::SchemaTooOld { .. })
            ));
        }
        assert!(matches!(
            classify_user_record(b"schema = [", 1, no_decode),
            Record::Malformed(MalformedRecord::Toml(_))
        ));
        assert!(matches!(
            classify_user_record(b"\xff", 1, no_decode),
            Record::Malformed(MalformedRecord::Encoding(_))
        ));
        // A newer schema still requires syntactically valid TOML first.
        assert!(matches!(
            classify_user_record(b"schema = 2\nbad = [", 1, no_decode),
            Record::Malformed(MalformedRecord::Toml(_))
        ));
    }
}
