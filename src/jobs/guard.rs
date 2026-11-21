//! Pure, platform-parameterised source capture accident check (storage §7).

use std::ffi::OsString;
use std::fmt;
use std::path::Path;

use crate::build::ExpandedArguments;
use crate::config::{Cwd, Placeholder, Platform, StageCommand, StagePlan, Token};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CaptureCheck {
    Checked,
    Unchecked { reasons: Vec<CaptureCheckReason> },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CaptureCheckReason {
    ShellStage,
}

/// A phase-3 configuration error. Native bytes are preserved for comparison;
/// only the diagnostic candidate is rendered lossily if it is not Unicode.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum GuardError {
    Placeholder(Placeholder),
    CwdSelector,
    CwdPath,
    /// The job directory is not lexically inside the workspace root as given:
    /// both must use the same spelling, or relative operands resolved against
    /// the job's cwd could never be compared with the workspace.
    JobOutsideWorkspace,
    Operand {
        element: OsString,
        candidate: String,
    },
}

impl fmt::Display for GuardError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("source capture accident guard (phase 3): ")?;
        match self {
            Self::Placeholder(value) => write!(f, "{{{}}} is forbidden", value.name()),
            Self::CwdSelector => f.write_str("cwd selector problem or workspace is forbidden"),
            Self::CwdPath => f.write_str("cwd is inside the workspace outside the job"),
            Self::JobOutsideWorkspace => {
                f.write_str("the job directory is not inside the workspace root as given")
            }
            Self::Operand { element, candidate } => write!(
                f,
                "element {element:?}, candidate {candidate:?} resolves inside the workspace outside the job"
            ),
        }
    }
}
impl std::error::Error for GuardError {}

/// Inspect raw tokens and expanded operands, never shell script text or env.
/// `cwd` is the actual expanded stage cwd. All paths are compared lexically;
/// neither this function nor its helpers access the filesystem.
pub fn check_capture(
    stage: &StagePlan,
    arguments: &ExpandedArguments,
    cwd: &Path,
    workspace: &Path,
    job: &Path,
    platform: Platform,
) -> Result<CaptureCheck, GuardError> {
    let texts = match &stage.command {
        StageCommand::Argv(argv) => argv,
        StageCommand::Shell { args, .. } => args,
    };
    let cwd_text = match &stage.cwd.value {
        Cwd::Problem | Cwd::Workspace => return Err(GuardError::CwdSelector),
        Cwd::Path(text) => Some(text),
        Cwd::Run => None,
    };
    for text in texts.iter().chain(cwd_text) {
        for token in &text.tokens {
            if let Token::Placeholder(value @ (Placeholder::ProblemDir | Placeholder::Workspace)) =
                token
            {
                return Err(GuardError::Placeholder(*value));
            }
        }
    }
    let cwd = lexical(cwd.as_os_str().as_encoded_bytes(), platform);
    let workspace = lexical(workspace.as_os_str().as_encoded_bytes(), platform);
    let job = lexical(job.as_os_str().as_encoded_bytes(), platform);
    if !job.inside(&workspace) {
        return Err(GuardError::JobOutsideWorkspace);
    }
    let forbidden = |path: &LexicalPath| path.inside(&workspace) && !path.inside(&job);
    if forbidden(&cwd) {
        return Err(GuardError::CwdPath);
    }
    let elements = match arguments {
        ExpandedArguments::Argv(elements) | ExpandedArguments::ShellArgs(elements) => elements,
    };
    for element in elements {
        let bytes = element.as_encoded_bytes();
        let after_equals = bytes
            .iter()
            .position(|b| *b == b'=')
            .map(|i| &bytes[i + 1..]);
        let short_option = bytes.len() > 2
            && ((bytes[0] == b'-' && bytes[1] != b'-')
                || (platform == Platform::Windows && bytes[0] == b'/' && bytes[1] != b'/'));
        for candidate in std::iter::once(bytes)
            .chain(after_equals)
            .chain(short_option.then(|| &bytes[2..]))
        {
            if !candidate.iter().any(|b| separator(*b, platform)) {
                continue;
            }
            let mut path = lexical(candidate, platform);
            if path.root.is_empty() {
                path = cwd.clone();
                // A Windows rooted operand keeps the cwd's drive/share but
                // replaces its directory components (it is not drive-absolute).
                if platform == Platform::Windows
                    && candidate.first().is_some_and(|b| separator(*b, platform))
                {
                    path.parts.clear();
                }
                path.apply(candidate, platform);
            }
            if forbidden(&path) {
                return Err(GuardError::Operand {
                    element: element.clone(),
                    candidate: String::from_utf8_lossy(candidate).into_owned(),
                });
            }
        }
    }
    Ok(match &stage.command {
        StageCommand::Argv(_) => CaptureCheck::Checked,
        StageCommand::Shell { .. } => CaptureCheck::Unchecked {
            reasons: vec![CaptureCheckReason::ShellStage],
        },
    })
}

fn separator(byte: u8, platform: Platform) -> bool {
    byte == b'/' || (platform == Platform::Windows && byte == b'\\')
}

#[derive(Clone, Debug)]
struct LexicalPath {
    root: Vec<u8>,
    parts: Vec<Vec<u8>>,
}

impl LexicalPath {
    fn apply(&mut self, tail: &[u8], platform: Platform) {
        for part in tail.split(|b| separator(*b, platform)) {
            match part {
                b"" | b"." => {}
                b".." => {
                    self.parts.pop();
                }
                _ => self.parts.push(if platform == Platform::Linux {
                    part.to_vec()
                } else {
                    part.to_ascii_lowercase()
                }),
            }
        }
    }

    fn inside(&self, parent: &Self) -> bool {
        self.root == parent.root && self.parts.starts_with(&parent.parts)
    }
}

fn lexical(mut bytes: &[u8], platform: Platform) -> LexicalPath {
    let mut root = Vec::new();
    // Canonical Windows job paths use verbatim prefixes. Treat ordinary and
    // verbatim drive/UNC spellings alike without resolving filesystem aliases.
    if platform == Platform::Windows {
        if bytes
            .get(..8)
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case(b"\\\\?\\UNC\\"))
        {
            bytes = &bytes[8..];
            root = b"unc".to_vec();
        } else if bytes.starts_with(b"\\\\?\\") {
            bytes = &bytes[4..];
        }
        if root.is_empty() && bytes.len() >= 3 && bytes[1] == b':' && separator(bytes[2], platform)
        {
            root = bytes[..2].to_ascii_lowercase();
            bytes = &bytes[3..];
        } else if root.as_slice() == b"unc"
            || (bytes.len() >= 2 && separator(bytes[0], platform) && separator(bytes[1], platform))
        {
            if root.is_empty() {
                bytes = &bytes[2..];
            }
            root = b"unc".to_vec();
            // Server and share belong to the root: .. cannot remove them.
            for _ in 0..2 {
                let end = bytes
                    .iter()
                    .position(|b| separator(*b, platform))
                    .unwrap_or(bytes.len());
                root.push(b'/');
                root.extend(bytes[..end].to_ascii_lowercase());
                bytes = bytes.get(end + 1..).unwrap_or_default();
            }
        }
    } else if bytes.starts_with(b"/") {
        root.push(b'/');
    }
    let mut path = LexicalPath {
        root,
        parts: Vec::new(),
    };
    path.apply(bytes, platform);
    path
}

#[cfg(test)]
mod tests;
