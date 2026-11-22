//! Storage contract primitives. Acquire one [`WorkspaceGuard`] per outer operation,
//! inspect its recovery warning, and borrow it for all mutation and staging steps.
//! Create structural directories through [`WorkspaceGuard::create_dir_all`] so
//! publication can sync every newly created ancestor and its parent.
//!
//! An unchanged edit succeeds with `committed = false` and performs no write.
//! A successful publication is always committed, even if durability or cleanup
//! needs a warning. Never retry it because of those warnings. Replacements copy
//! Unix mode only; ownership, ACLs, extended attributes and Windows attributes
//! are inherited from the destination directory rather than preserved.

pub mod admission;
pub mod budget;
pub(crate) mod crash;
mod edit;
pub mod facts;
pub mod inputs;
pub mod job;
pub mod job_seq;
mod lock;
pub mod paths;
pub mod prune;
mod publish;
mod record;
mod recovery;
mod sys;
mod walk;

use std::fmt;
use std::io;
use std::path::PathBuf;

pub use edit::{EditError, edit_text, edit_toml, edit_toml_replay};
pub(crate) use lock::checked_path;
pub use lock::{LockOptions, WorkspaceGuard, WorkspaceLock};
pub(crate) use publish::copy_stream;
pub use publish::{
    atomic_replace, publish_dir_noclobber, publish_file_noclobber,
    publish_file_noclobber_from_reader,
};
pub use record::{
    MalformedRecord, Record, classify_tool_record, classify_user_record,
    classify_user_record_spanned, read_tool_record, read_user_record,
};
pub(crate) use record::{RECORD_SIZE_LIMIT, RecordBytes, read_record_bytes};
pub(crate) use sys::is_link;
pub use walk::{SkipReason, Skipped, WalkReport, measure_tree, remove_tree};
pub(crate) use walk::{measure_owned_tree, remove_owned_tree};

#[derive(Debug)]
pub struct Published {
    pub committed: bool,
    pub durability: Durability,
    pub cleanup: Cleanup,
}

#[derive(Debug)]
pub enum Durability {
    Ok,
    Uncertain(io::Error),
}

#[derive(Debug, PartialEq, Eq)]
pub enum Cleanup {
    Done,
    Pending(PathBuf),
}

/// All variants describe a failure before commit. The destination is unchanged.
#[derive(Debug)]
pub enum ErrorKind {
    Io {
        path: PathBuf,
        source: io::Error,
    },
    Exists {
        path: PathBuf,
    },
    CrossFilesystem {
        source: PathBuf,
        target: PathBuf,
    },
    Unsupported {
        op: &'static str,
        filesystem: String,
    },
    PathTooLong {
        path: PathBuf,
    },
    LinkInPath {
        path: PathBuf,
    },
    InvalidPath {
        path: PathBuf,
    },
    InvalidName(crate::names::NameError),
    Busy {
        op: String,
    },
    Conflict(Conflict),
}

#[derive(Debug)]
pub struct StorageError {
    pub kind: ErrorKind,
    pub cleanup: Cleanup,
}

impl StorageError {
    /// Errors never represent an already committed publication.
    pub const fn committed(&self) -> bool {
        false
    }

    pub(crate) fn new(kind: ErrorKind) -> Self {
        Self {
            kind,
            cleanup: Cleanup::Done,
        }
    }

    pub(crate) fn io(path: impl Into<PathBuf>, source: io::Error) -> Self {
        let path = path.into();
        if sys::name_too_long(&source, &path) {
            Self::new(ErrorKind::PathTooLong { path })
        } else {
            Self::new(ErrorKind::Io { path, source })
        }
    }
}

impl fmt::Display for StorageError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.kind)?;
        if let Cleanup::Pending(path) = &self.cleanup {
            write!(f, "; cleanup pending: {}", path.display())?;
        }
        Ok(())
    }
}

impl fmt::Display for ErrorKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io { path, source } => write!(f, "{}: {source}", path.display()),
            Self::Exists { path } => write!(f, "{} already exists", path.display()),
            Self::CrossFilesystem { source, target } => write!(
                f,
                "{} and {} are on different filesystems",
                source.display(),
                target.display()
            ),
            Self::Unsupported { op, filesystem } => {
                write!(f, "{op} is not supported on this filesystem ({filesystem})")
            }
            Self::PathTooLong { path } => write!(f, "path too long: {}", path.display()),
            Self::LinkInPath { path } => {
                write!(f, "refusing a link or junction at {}", path.display())
            }
            Self::InvalidPath { path } => write!(f, "invalid path: {}", path.display()),
            Self::InvalidName(error) => write!(f, "invalid name: {error}"),
            Self::Busy { op } => {
                write!(f, "workspace is busy (another cpf holds the lock for {op})")
            }
            Self::Conflict(conflict) => write!(f, "{conflict}"),
        }
    }
}

impl std::error::Error for StorageError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match &self.kind {
            ErrorKind::Io { source, .. } => Some(source),
            ErrorKind::InvalidName(source) => Some(source),
            ErrorKind::Conflict(source) => Some(source),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Conflict {
    pub path: PathBuf,
}

impl fmt::Display for Conflict {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "concurrent edit of {}", self.path.display())
    }
}

impl std::error::Error for Conflict {}

/// Recovery failures are warnings; acquisition and the operation continue.
/// `paths` and `errors` have matching indexes.
#[derive(Debug, Default)]
pub struct RecoveryIncomplete {
    pub paths: Vec<PathBuf>,
    pub errors: Vec<io::Error>,
}

impl RecoveryIncomplete {
    pub(crate) fn push(&mut self, path: PathBuf, error: io::Error) {
        self.paths.push(path);
        self.errors.push(error);
    }
}

/// Failure points available only to integration-test builds with the explicit
/// `storage-test-hooks` feature. Configuration belongs to a single guard.
#[cfg(feature = "storage-test-hooks")]
#[derive(Clone, Debug, Default)]
pub struct TestHooks {
    pub post_commit_sync_failure: bool,
    pub force_link_fallback: bool,
    pub fail_unlink: bool,
    pub fail_pre_commit_sync: bool,
    pub fail_write_disk_full: bool,
    pub fail_publish_permission_denied: bool,
    /// Simulate an independent creator after temp preparation, before a file
    /// no-clobber publication attempts to commit.
    pub before_file_publish: Option<fn(&std::path::Path)>,
    /// Replace a registration input after path inspection, before its open.
    pub test_input_before_open: Option<fn(&std::path::Path)>,
    /// Simulate an editor after the baseline read, before parsing/checking it.
    pub edit_after_read: Option<fn(&std::path::Path)>,
    /// Simulate an editor after a failed native replacement attempt.
    pub replace_after_failure: Option<fn(&std::path::Path, usize)>,
    pub first_temp_random: Option<[u8; 16]>,
    pub force_directory_unsupported: bool,
    pub force_cross_filesystem: bool,
    pub recovery_remove_failure: Option<PathBuf>,
    pub recovery_read_failure: Option<PathBuf>,
    pub walker_mount_point: Option<PathBuf>,
    pub walker_device_boundary: Option<PathBuf>,
    /// Refuse pruning renames without changing the source or destination.
    pub fail_prune_rename: bool,
}
