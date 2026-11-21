use std::collections::HashMap;
use std::fmt;
use std::fs::File;
use std::io;
use std::path::{Path, PathBuf};

use super::{InputsError, JobInputs, relative_path};
use crate::config::{ProblemPath, TargetPlan};
use crate::storage::{ErrorKind, StorageError, checked_path};
use crate::tests_store::{Health, open_regular};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SourceRole {
    Source,
    Support,
}

/// One validated phase-1 declaration. The caller preserves target order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CapturedSource {
    pub role: SourceRole,
    pub problem_path: ProblemPath,
}
impl CapturedSource {
    pub fn from_target(target: &TargetPlan) -> Vec<Self> {
        target
            .captured_files()
            .enumerate()
            .map(|(index, file)| Self {
                role: if index < target.sources.len() {
                    SourceRole::Source
                } else {
                    SourceRole::Support
                },
                problem_path: file.value.clone(),
            })
            .collect()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CapturedSourceCopy {
    pub role: SourceRole,
    pub problem_path: ProblemPath,
    pub copy_path: PathBuf,
    pub len: u64,
}

#[derive(Debug)]
pub struct CapturedSources {
    files: Vec<CapturedSourceCopy>,
}
impl CapturedSources {
    pub fn files(&self) -> &[CapturedSourceCopy] {
        &self.files
    }
    /// Bindings for {sources}, excluding support files, in configured order.
    pub fn source_bindings(&self) -> Vec<PathBuf> {
        self.files
            .iter()
            .filter(|file| file.role == SourceRole::Source)
            .map(|file| file.copy_path.clone())
            .collect()
    }
}

#[derive(Debug)]
pub enum CaptureError {
    Inputs(InputsError),
    Unhealthy { path: PathBuf, health: Health },
    ReservedPath { path: PathBuf },
    SameFile { first: PathBuf, second: PathBuf },
    SourceCollision { path: PathBuf },
}
impl fmt::Display for CaptureError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Inputs(error) => write!(f, "{error}"),
            Self::Unhealthy { path, health } => write!(f, "{}: {health}", path.display()),
            Self::ReservedPath { path } => {
                write!(f, "source is inside reserved storage: {}", path.display())
            }
            Self::SameFile { first, second } => write!(
                f,
                "sources name the same file: {} and {}",
                first.display(),
                second.display()
            ),
            Self::SourceCollision { path } => {
                write!(f, "source destination already exists: {}", path.display())
            }
        }
    }
}
impl std::error::Error for CaptureError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Inputs(error) => Some(error),
            _ => None,
        }
    }
}
impl From<InputsError> for CaptureError {
    fn from(error: InputsError) -> Self {
        Self::Inputs(error)
    }
}
impl From<StorageError> for CaptureError {
    fn from(error: StorageError) -> Self {
        Self::Inputs(error.into())
    }
}

/// Preflight every declaration before copying any source. Keep the opened
/// handles so editor replacements after validation cannot change file identity.
/// Filesystems still decide destination aliases via exclusive creation.
pub fn capture_sources(
    target_files: &[CapturedSource],
    problem_dir: &Path,
    inputs: &JobInputs<'_>,
) -> Result<CapturedSources, CaptureError> {
    inputs.finish(capture_checked(target_files, problem_dir, inputs))
}

fn capture_checked(
    target_files: &[CapturedSource],
    problem_dir: &Path,
    inputs: &JobInputs<'_>,
) -> Result<CapturedSources, CaptureError> {
    inputs.ensure_active()?;
    let mut seen = HashMap::new();
    let mut opened = Vec::with_capacity(target_files.len());
    for source in target_files {
        let relative = relative_path(&source.problem_path);
        let path = problem_dir.join(&relative);
        let components = source.problem_path.components();
        if components
            .first()
            .is_some_and(|part| part.eq_ignore_ascii_case(".cpforge"))
            && components.get(1).is_some_and(|part| {
                [
                    "runs",
                    "cache",
                    "staging",
                    "facts",
                    "state.toml",
                    "lock",
                    "job-seq",
                ]
                .iter()
                .any(|name| part.eq_ignore_ascii_case(name))
            })
        {
            return Err(CaptureError::ReservedPath { path });
        }
        checked_path(problem_dir, &relative, false)?;
        let file = open_regular(&path).map_err(|health| CaptureError::Unhealthy {
            path: path.clone(),
            health,
        })?;
        let identity = file_identity(&file).map_err(|error| StorageError::io(&path, error))?;
        if let Some(first) = seen.insert(identity, path.clone()) {
            return Err(CaptureError::SameFile {
                first,
                second: path,
            });
        }
        opened.push((source, relative, file));
    }
    let mut files = Vec::with_capacity(opened.len());
    for (source, relative, mut file) in opened {
        let copy = inputs
            .copy(&Path::new("src").join(relative), &mut file)
            .map_err(|error| match error {
                InputsError::Storage(StorageError {
                    kind: ErrorKind::Io { path, source },
                    ..
                }) if source.kind() == io::ErrorKind::AlreadyExists => {
                    CaptureError::SourceCollision { path }
                }
                error => CaptureError::Inputs(error),
            })?;
        files.push(CapturedSourceCopy {
            role: source.role,
            problem_path: source.problem_path.clone(),
            copy_path: copy.path,
            len: copy.len,
        });
    }
    Ok(CapturedSources { files })
}

#[cfg(unix)]
fn file_identity(file: &File) -> io::Result<(u64, u64)> {
    use std::os::unix::fs::MetadataExt;
    let metadata = file.metadata()?;
    Ok((metadata.dev(), metadata.ino()))
}

#[cfg(windows)]
fn file_identity(file: &File) -> io::Result<(u64, u64)> {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Storage::FileSystem::{
        BY_HANDLE_FILE_INFORMATION, GetFileInformationByHandle,
    };
    let mut info = BY_HANDLE_FILE_INFORMATION::default();
    // SAFETY: the owned handle is live and info is writable for this call.
    if unsafe { GetFileInformationByHandle(file.as_raw_handle(), &mut info) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok((
        u64::from(info.dwVolumeSerialNumber),
        (u64::from(info.nFileIndexHigh) << 32) | u64::from(info.nFileIndexLow),
    ))
}
