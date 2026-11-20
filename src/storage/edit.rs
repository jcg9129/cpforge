//! Optimistic shared-record edits under a borrowed workspace lock.

use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::Read;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};
use toml_edit::DocumentMut;

use super::{
    Cleanup, Conflict, ErrorKind, Published, StorageError, WorkspaceGuard, atomic_replace, sys,
};

/// Every error is pre-commit. Conflicts retain any failed temp cleanup warning.
#[derive(Debug)]
pub enum EditError<E> {
    Storage(StorageError),
    Conflict {
        conflict: Conflict,
        cleanup: Cleanup,
        /// A temp from the first attempt whose removal failed.
        previous_cleanup: Cleanup,
    },
    /// A non-conflict error on replay, with the first attempt's cleanup warning.
    Replay {
        error: Box<EditError<E>>,
        cleanup: Cleanup,
    },
    NotSingleRegularFile {
        path: PathBuf,
    },
    Encoding(std::str::Utf8Error),
    Parse(toml_edit::TomlError),
    Check(E),
    Operations(E),
}

impl<E: fmt::Display> fmt::Display for EditError<E> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Storage(error) => fmt::Display::fmt(error, f),
            Self::Conflict {
                conflict,
                cleanup,
                previous_cleanup,
            } => {
                write!(
                    f,
                    "{conflict}; cleanup: {cleanup:?}; previous cleanup: {previous_cleanup:?}"
                )
            }
            Self::Replay { error, cleanup } => {
                write!(f, "{error}; previous cleanup: {cleanup:?}")
            }
            Self::NotSingleRegularFile { path } => {
                write!(f, "{} is not a regular file with one link", path.display())
            }
            Self::Encoding(error) => fmt::Display::fmt(error, f),
            Self::Parse(error) => fmt::Display::fmt(error, f),
            Self::Check(error) => write!(f, "record check failed: {error}"),
            Self::Operations(error) => write!(f, "record edit failed: {error}"),
        }
    }
}

impl<E: std::error::Error + 'static> std::error::Error for EditError<E> {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Storage(error) => Some(error),
            Self::Conflict { conflict, .. } => Some(conflict),
            Self::Replay { error, .. } => Some(error.as_ref()),
            Self::Encoding(error) => Some(error),
            Self::Parse(error) => Some(error),
            Self::Check(error) | Self::Operations(error) => Some(error),
            Self::NotSingleRegularFile { .. } => None,
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
struct Baseline {
    bytes: Vec<u8>,
    length: u64,
    hash: [u8; 32],
    identity: [u64; 2],
    file_type: fs::FileType,
    links: u64,
}

fn read_baseline<E>(guard: &WorkspaceGuard<'_>, path: &Path) -> Result<Baseline, EditError<E>> {
    let path = guard.check_path(path).map_err(EditError::Storage)?;
    let metadata = fs::symlink_metadata(&path)
        .map_err(|error| EditError::Storage(StorageError::io(&path, error)))?;
    if sys::is_link(&metadata) || !metadata.is_file() {
        return Err(EditError::NotSingleRegularFile { path });
    }
    let mut open = OpenOptions::new();
    open.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        // O_NONBLOCK also prevents a raced FIFO from blocking before fstat.
        open.custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        use windows_sys::Win32::Storage::FileSystem::{
            FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT,
        };
        open.custom_flags(FILE_FLAG_OPEN_REPARSE_POINT | FILE_FLAG_BACKUP_SEMANTICS);
    }
    let mut file = open
        .open(&path)
        .map_err(|error| EditError::Storage(StorageError::io(&path, error)))?;
    let metadata = file
        .metadata()
        .map_err(|error| EditError::Storage(StorageError::io(&path, error)))?;
    let (identity, links) = identity_and_links(&file)
        .map_err(|error| EditError::Storage(StorageError::io(&path, error)))?;
    if sys::is_link(&metadata) || !metadata.is_file() || links != 1 {
        return Err(EditError::NotSingleRegularFile { path });
    }
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)
        .map_err(|error| EditError::Storage(StorageError::io(&path, error)))?;
    Ok(Baseline {
        hash: Sha256::digest(&bytes).into(),
        bytes,
        length: metadata.len(),
        identity,
        file_type: metadata.file_type(),
        links,
    })
}

fn identity_and_links(file: &File) -> std::io::Result<([u64; 2], u64)> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let metadata = file.metadata()?;
        Ok(([metadata.dev(), metadata.ino()], metadata.nlink()))
    }
    #[cfg(windows)]
    {
        use std::os::windows::io::AsRawHandle;
        use windows_sys::Win32::Storage::FileSystem::{
            BY_HANDLE_FILE_INFORMATION, GetFileInformationByHandle,
        };
        let mut info = BY_HANDLE_FILE_INFORMATION::default();
        // SAFETY: file owns a live handle; info is writable for this call.
        if unsafe { GetFileInformationByHandle(file.as_raw_handle(), &mut info) } == 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok((
            [
                u64::from(info.dwVolumeSerialNumber),
                (u64::from(info.nFileIndexHigh) << 32) | u64::from(info.nFileIndexLow),
            ],
            u64::from(info.nNumberOfLinks),
        ))
    }
}

/// Edit a fresh TOML document, checking its schema before applying operations.
/// Untouched items retain their original formatting and comments. Neither this
/// primitive nor its caller callbacks retry on Conflict. `check` and `ops` must
/// be replayable functions without external side effects (also when used with
/// [`edit_toml_replay`]); edits belong only in the supplied document.
///
/// Final validation compares bytes, length, SHA-256, identity, type and link
/// count before every rename attempt. This is not compare-and-swap: an editor
/// can still save between validation and rename. Cooperating library users are
/// serialized by the guard. Unix mode is preserved by [`atomic_replace`].
pub fn edit_toml<E>(
    guard: &WorkspaceGuard<'_>,
    path: impl AsRef<Path>,
    check: impl Fn(&DocumentMut) -> Result<(), E>,
    ops: impl Fn(&mut DocumentMut) -> Result<(), E>,
) -> Result<Published, EditError<E>> {
    let path = guard.check_path(path).map_err(EditError::Storage)?;
    let baseline = read_baseline(guard, &path)?;
    #[cfg(feature = "storage-test-hooks")]
    if let Some(hook) = guard.test_hooks.edit_after_read {
        hook(&path);
    }
    let text = std::str::from_utf8(&baseline.bytes).map_err(EditError::Encoding)?;
    let mut document = text.parse::<DocumentMut>().map_err(EditError::Parse)?;
    check(&document).map_err(EditError::Check)?;
    let before = document.to_string();
    ops(&mut document).map_err(EditError::Operations)?;
    let rendered = document.to_string();
    let output = if before == rendered {
        text.to_owned()
    } else {
        // Keep the original spelling only when the result is still the same
        // document; otherwise publish toml_edit's own rendering.
        let preserved = preserve_lines(text, &rendered);
        let same = preserved.parse::<DocumentMut>().is_ok_and(|parsed| {
            parsed.to_string().replace("\r\n", "\n") == rendered.replace("\r\n", "\n")
        });
        if same { preserved } else { rendered }
    };
    let unchanged = || read_baseline::<E>(guard, &path).is_ok_and(|current| current == baseline);
    let result = atomic_replace(guard, &path, output.as_bytes(), || {
        if !unchanged() {
            return Err(Conflict { path: path.clone() });
        }
        super::crash::point("edit_toml.validated");
        Ok(())
    });
    match result {
        Ok(published) => {
            super::crash::point("edit_toml.published");
            Ok(published)
        }
        Err(error) => {
            // A type change may be noticed while preparing the temp (e.g. when
            // copying its Unix mode), before the final validation is reached.
            if matches!(&error.kind, ErrorKind::Conflict(_)) || !unchanged() {
                Err(EditError::Conflict {
                    conflict: Conflict { path },
                    cleanup: error.cleanup,
                    previous_cleanup: Cleanup::Done,
                })
            } else {
                Err(EditError::Storage(error))
            }
        }
    }
}

/// Explicitly opt into one replay after Conflict: at most two attempts total.
/// Each attempt reads afresh, parses and rechecks the schema, then reapplies the
/// operations. Both callbacks must have no external side effects. A second
/// Conflict is returned. Cleanup warnings from the first attempt survive in
/// the publication result, the second Conflict, or [`EditError::Replay`].
pub fn edit_toml_replay<E>(
    guard: &WorkspaceGuard<'_>,
    path: impl AsRef<Path>,
    check: impl Fn(&DocumentMut) -> Result<(), E>,
    ops: impl Fn(&mut DocumentMut) -> Result<(), E>,
) -> Result<Published, EditError<E>> {
    let path = path.as_ref();
    let cleanup = match edit_toml(guard, path, &check, &ops) {
        Err(EditError::Conflict { cleanup, .. }) => cleanup,
        result => return result,
    };
    match edit_toml(guard, path, check, ops) {
        Ok(mut published) => {
            // Replacement commits by rename, so its own temp name is gone.
            published.cleanup = cleanup;
            Ok(published)
        }
        Err(mut error) => {
            if cleanup == Cleanup::Done {
                return Err(error);
            }
            if let EditError::Conflict {
                previous_cleanup, ..
            } = &mut error
            {
                *previous_cleanup = cleanup;
                Err(error)
            } else {
                Err(EditError::Replay {
                    error: Box::new(error),
                    cleanup,
                })
            }
        }
    }
}

// toml_edit normalizes CRLF and supplies a final newline. Restore the exact
// original spelling of unchanged lines, including multiline values and comments.
// New/changed lines use the original document's first newline style.
fn preserve_lines(original: &str, rendered: &str) -> String {
    use std::collections::{HashMap, VecDeque};

    let (bom, original) = original
        .strip_prefix('\u{feff}')
        .map_or(("", original), |text| ("\u{feff}", text));
    let newline = original.find('\n').map_or("\n", |index| {
        if original[..index].ends_with('\r') {
            "\r\n"
        } else {
            "\n"
        }
    });
    let mut lines: HashMap<String, VecDeque<&str>> = HashMap::new();
    for line in original.split_inclusive('\n') {
        lines
            .entry(line_key(line).to_owned())
            .or_default()
            .push_back(line);
    }
    let mut output = String::from(bom);
    let mut rendered_lines = rendered.split_inclusive('\n').peekable();
    while let Some(line) = rendered_lines.next() {
        // Rendered decor may keep an original `\r` before the newline; it is
        // never written again in front of the chosen newline.
        let key = line_key(line);
        if let Some(raw) = lines.get_mut(key).and_then(VecDeque::pop_front) {
            output.push_str(raw);
            if !raw.ends_with('\n') && rendered_lines.peek().is_some() {
                output.push_str(newline);
            }
        } else {
            output.push_str(key);
            if line.ends_with('\n') {
                output.push_str(newline);
            }
        }
    }
    output
}

// A line without its terminator: the trailing `\n` and one `\r` before it.
fn line_key(line: &str) -> &str {
    let line = line.strip_suffix('\n').unwrap_or(line);
    line.strip_suffix('\r').unwrap_or(line)
}
