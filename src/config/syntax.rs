use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};

use super::Platform;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Placeholder {
    Sources,
    Source,
    Artifact,
    ArtifactDir,
    Entry,
    ProblemDir,
    ProblemId,
    Workspace,
    TestInput,
    TestAnswer,
    Iteration,
}

impl Placeholder {
    /// The name as written between braces, e.g. `test_input`.
    pub fn name(self) -> &'static str {
        match self {
            Self::Sources => "sources",
            Self::Source => "source",
            Self::Artifact => "artifact",
            Self::ArtifactDir => "artifact_dir",
            Self::Entry => "entry",
            Self::ProblemDir => "problem_dir",
            Self::ProblemId => "problem_id",
            Self::Workspace => "workspace",
            Self::TestInput => "test_input",
            Self::TestAnswer => "test_answer",
            Self::Iteration => "iteration",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Token {
    Literal(String),
    Placeholder(Placeholder),
}

/// Tokenize a single pass; errors are returned together in source order.
pub fn tokenize(text: &str) -> Result<Vec<Token>, Vec<String>> {
    let mut tokens = Vec::new();
    let mut errors = Vec::new();
    let mut literal = String::new();
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '{' && c != '}' {
            literal.push(c);
        } else if chars.peek() == Some(&c) {
            chars.next();
            literal.push(c);
        } else if c == '}' {
            errors.push("lone closing brace".into());
        } else {
            let mut name = String::new();
            let mut closed = false;
            for next in chars.by_ref() {
                if next == '}' {
                    closed = true;
                    break;
                }
                name.push(next);
            }
            if !closed {
                errors.push("lone opening brace".into());
                break;
            }
            let placeholder = match name.as_str() {
                "sources" => Some(Placeholder::Sources),
                "source" => Some(Placeholder::Source),
                "artifact" => Some(Placeholder::Artifact),
                "artifact_dir" => Some(Placeholder::ArtifactDir),
                "entry" => Some(Placeholder::Entry),
                "problem_dir" => Some(Placeholder::ProblemDir),
                "problem_id" => Some(Placeholder::ProblemId),
                "workspace" => Some(Placeholder::Workspace),
                "test_input" => Some(Placeholder::TestInput),
                "test_answer" => Some(Placeholder::TestAnswer),
                "iteration" => Some(Placeholder::Iteration),
                _ => None,
            };
            if let Some(placeholder) = placeholder {
                if !literal.is_empty() {
                    tokens.push(Token::Literal(std::mem::take(&mut literal)));
                }
                tokens.push(Token::Placeholder(placeholder));
            } else {
                errors.push(format!("unknown placeholder {{{name}}}"));
            }
        }
    }
    if !literal.is_empty() {
        tokens.push(Token::Literal(literal));
    }
    if errors.is_empty() {
        Ok(tokens)
    } else {
        Err(errors)
    }
}

pub(super) fn overlay(name: &str) -> bool {
    matches!(name, "unix" | "linux" | "macos" | "windows")
}

pub(super) fn name_valid(name: &str, reserved: bool) -> bool {
    name.as_bytes().first().is_some_and(u8::is_ascii_alphabetic)
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
        && !(reserved && overlay(name))
}

fn drive(path: &str) -> bool {
    let b = path.as_bytes();
    b.len() >= 2 && b[0].is_ascii_alphabetic() && b[1] == b':'
}

pub(super) fn separator(path: &str, platform: Platform) -> bool {
    path.contains('/') || (platform == Platform::Windows && path.contains('\\'))
}

pub(super) fn absolute(path: &str, platform: Platform) -> bool {
    native_absolute(OsStr::new(path), platform)
}

pub(crate) fn native_absolute(path: &OsStr, platform: Platform) -> bool {
    // ASCII path syntax has the same bytes in native Unix and Windows encodings.
    let bytes = path.as_encoded_bytes();
    if platform != Platform::Windows {
        return bytes.first() == Some(&b'/');
    }
    let sep = |b: &u8| matches!(b, b'/' | b'\\');
    let drive_absolute =
        bytes.len() >= 3 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' && sep(&bytes[2]);
    let unc = if bytes.len() >= 2 && sep(&bytes[0]) && sep(&bytes[1]) {
        let mut parts = bytes[2..].split(sep);
        parts.next().is_some_and(|s| !s.is_empty()) && parts.next().is_some_and(|s| !s.is_empty())
    } else {
        false
    };
    drive_absolute || unc
}

pub(super) fn windows_relative(path: &str) -> bool {
    (drive(path) && !absolute(path, Platform::Windows))
        || ((path.starts_with('/') || path.starts_with('\\')) && !absolute(path, Platform::Windows))
}

/// Join using the validation platform, including when tests run on another OS.
/// Native strings are retained without lossy conversion.
pub(crate) fn join(base: &Path, tail: &OsStr, platform: Platform) -> PathBuf {
    let mut result = base.as_os_str().to_os_string();
    if !result.is_empty()
        && !result
            .as_encoded_bytes()
            .last()
            .is_some_and(|b| *b == b'/' || (platform == Platform::Windows && *b == b'\\'))
    {
        result.push(if platform == Platform::Windows {
            "\\"
        } else {
            "/"
        });
    }
    result.push(tail);
    PathBuf::from(result)
}

pub(super) fn directory(file: &Path, platform: Platform) -> PathBuf {
    if let Some(s) = file.to_str() {
        if let Some(pos) = s.rfind(|c| c == '/' || (platform == Platform::Windows && c == '\\')) {
            // Keep the separator of a drive root or Unix root.
            return PathBuf::from(
                &s[..if pos == 0 || (platform == Platform::Windows && pos == 2 && drive(s)) {
                    pos + 1
                } else {
                    pos
                }],
            );
        }
        return PathBuf::new();
    }
    // Non-Unicode file names use the native platform's component representation.
    file.parent().unwrap_or_else(|| Path::new("")).to_owned()
}

pub(super) fn home_expand(
    text: &str,
    home: Option<&Path>,
    platform: Platform,
) -> Result<OsString, &'static str> {
    if text.starts_with("~/") || (platform == Platform::Windows && text.starts_with("~\\")) {
        let home = home.ok_or("home directory is unavailable for ~ expansion")?;
        Ok(join(home, OsStr::new(&text[2..]), platform).into_os_string())
    } else {
        Ok(OsString::from(text))
    }
}

pub(super) fn output_relative(text: &str, platform: Platform) -> bool {
    !text.is_empty()
        && !text.contains('\0')
        && !text.starts_with('/')
        && !(platform == Platform::Windows && (text.starts_with('\\') || drive(text)))
        && !text
            .split(|c| c == '/' || (platform == Platform::Windows && c == '\\'))
            .any(|c| c == "..")
}

pub(super) fn source_valid(text: &str, platform: Platform) -> bool {
    !text.is_empty()
        && !text.contains('\0')
        && !(platform == Platform::Windows && (text.contains('\\') || drive(text)))
        && text.split('/').all(|c| !matches!(c, "" | "." | ".."))
}

pub(super) fn ignore_valid(text: &str) -> bool {
    !text.is_empty()
        && !text.contains(['[', ']', '!', '\\', '\0'])
        && text
            .split('/')
            .all(|c| !matches!(c, "" | "." | "..") && (!c.contains("**") || c == "**"))
}
