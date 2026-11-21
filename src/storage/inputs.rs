//! Frozen inputs under a registered job's held lease. No execution capability.

use std::fmt;
use std::fs::{self, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::Ordering;

use sha2::{Digest, Sha256};

use super::admission::RegisteredJob;
use super::budget::BudgetExceeded;
use super::{ErrorKind, StorageError, checked_path};
use crate::config::ProblemPath;
use crate::tests_store::{FreezeError, FrozenManifest, TestId, TestStore};

mod sources;
pub use sources::{
    CaptureError, CapturedSource, CapturedSourceCopy, CapturedSources, SourceRole, capture_sources,
};

/// Only registration can grant this capability. Borrowing keeps the lease and
/// accounting alive; all destinations are checked below this job's inputs/.
#[derive(Debug)]
pub struct JobInputs<'a> {
    job: &'a RegisteredJob,
}

impl RegisteredJob {
    pub fn inputs(&self) -> JobInputs<'_> {
        JobInputs { job: self }
    }
}

/// Digest and length describe the bytes actually written, even if the original
/// changed during copying. Paths are absolute paths to the job-owned copies.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FrozenFile {
    pub path: PathBuf,
    pub len: u64,
    pub sha256: [u8; 32],
}

/// Complete tests and sources for one job. Construction requires both phases.
/// C10a5 combines RegisteredJob + FrozenInputs + the plan into ReadyJob;
/// neither registration nor these inputs alone grants permission to run.
#[derive(Debug)]
pub struct FrozenInputs {
    job_dir: PathBuf,
    manifest: FrozenManifest,
    sources: CapturedSources,
}

impl FrozenInputs {
    pub fn job_dir(&self) -> &Path {
        &self.job_dir
    }
    pub fn manifest(&self) -> &FrozenManifest {
        &self.manifest
    }
    pub fn sources(&self) -> &CapturedSources {
        &self.sources
    }
}

#[derive(Debug)]
pub enum InputsError {
    Storage(StorageError),
    Budget(BudgetExceeded),
    Failed,
}

impl fmt::Display for InputsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Storage(error) => write!(f, "{error}"),
            Self::Budget(error) => write!(f, "{error}"),
            Self::Failed => f.write_str("this job's input freeze has already failed"),
        }
    }
}
impl std::error::Error for InputsError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Storage(error) => Some(error),
            Self::Budget(error) => Some(error),
            Self::Failed => None,
        }
    }
}
impl From<StorageError> for InputsError {
    fn from(error: StorageError) -> Self {
        Self::Storage(error)
    }
}

#[derive(Debug)]
pub enum FreezeInputsError {
    Tests(FreezeError),
    Sources(CaptureError),
}
impl fmt::Display for FreezeInputsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Tests(error) => write!(f, "{error}"),
            Self::Sources(error) => write!(f, "{error}"),
        }
    }
}
impl std::error::Error for FreezeInputsError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Tests(error) => Some(error),
            Self::Sources(error) => Some(error),
        }
    }
}

impl JobInputs<'_> {
    /// Freeze tests first, then capture the target's sources and support files.
    /// Failure retains partial storage for pruning and permanently fails this job.
    pub fn freeze(
        &self,
        store: &TestStore,
        ids: &[TestId],
        target_files: &[CapturedSource],
        problem_dir: &Path,
    ) -> Result<FrozenInputs, FreezeInputsError> {
        let manifest = store
            .freeze_into(ids, self)
            .map_err(FreezeInputsError::Tests)?;
        let sources =
            capture_sources(target_files, problem_dir, self).map_err(FreezeInputsError::Sources)?;
        Ok(FrozenInputs {
            job_dir: self.job.dir.clone(),
            manifest,
            sources,
        })
    }

    pub(crate) fn ensure_active(&self) -> Result<(), InputsError> {
        if self.job.inputs_failed.load(Ordering::Relaxed) {
            Err(InputsError::Failed)
        } else {
            Ok(())
        }
    }

    pub(crate) fn finish<T, E>(&self, result: Result<T, E>) -> Result<T, E> {
        if result.is_err() {
            self.job.inputs_failed.store(true, Ordering::Relaxed);
        }
        result
    }

    pub(crate) fn copy(
        &self,
        relative: &Path,
        reader: &mut impl Read,
    ) -> Result<FrozenFile, InputsError> {
        let result = self.copy_inner(relative, reader);
        self.finish(result)
    }

    fn copy_inner(
        &self,
        relative: &Path,
        reader: &mut impl Read,
    ) -> Result<FrozenFile, InputsError> {
        self.ensure_active()?;
        if relative.as_os_str().is_empty()
            || !relative
                .components()
                .all(|part| matches!(part, Component::Normal(_)))
        {
            return Err(StorageError::new(ErrorKind::InvalidPath {
                path: relative.into(),
            })
            .into());
        }
        let relative = Path::new("inputs").join(relative);
        // Check from the job root, including inputs itself, before creating any
        // parent. Stable structural ancestors are the storage contract's premise.
        let path = checked_path(&self.job.dir, &relative, false)?;
        let parent = path.parent().expect("input has a parent");
        fs::create_dir_all(parent).map_err(|error| StorageError::io(parent, error))?;
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .map_err(|error| StorageError::io(&path, error))?;
        let mut hash = Sha256::new();
        let mut len = 0_u64;
        let mut buffer = [0_u8; 64 * 1024];
        loop {
            let count = match reader.read(&mut buffer) {
                Ok(0) => break,
                Ok(count) => count,
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(error) => return Err(StorageError::io(&path, error).into()),
            };
            // Never trust an initial metadata length: a source may keep growing.
            // No allocation occurs before reserving this entire chunk.
            let mut reservation = self
                .job
                .budget
                .reserve(count as u64)
                .map_err(InputsError::Budget)?;
            let (written, result) = write_chunk(&mut file, &buffer[..count], &mut hash);
            // A failed write can still leave bytes. Retain every successful
            // write, releasing only unused headroom; partial files stay for prune.
            reservation
                .commit(written as u64)
                .expect("writes fit their chunk reservation");
            result.map_err(|error| StorageError::io(&path, error))?;
            // The shared budget bounds the sum, including this addition.
            len = len
                .checked_add(written as u64)
                .expect("copy length fits the shared budget");
        }
        Ok(FrozenFile {
            path,
            len,
            sha256: hash.finalize().into(),
        })
    }
}

/// Write one reserved chunk, hashing exactly the bytes the file accepted.
/// The count is returned even on failure so partial writes stay accounted.
fn write_chunk(file: &mut fs::File, chunk: &[u8], hash: &mut Sha256) -> (usize, io::Result<()>) {
    let mut written = 0;
    while written < chunk.len() {
        match file.write(&chunk[written..]) {
            Ok(0) => {
                return (
                    written,
                    Err(io::Error::new(
                        io::ErrorKind::WriteZero,
                        "input copy made no progress",
                    )),
                );
            }
            Ok(n) => {
                hash.update(&chunk[written..written + n]);
                written += n;
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return (written, Err(error)),
        }
    }
    (written, Ok(()))
}

fn relative_path(path: &ProblemPath) -> PathBuf {
    path.components().iter().collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::WorkspaceLock;
    use crate::storage::admission::register_job;
    use crate::storage::prune::PrunePolicy;
    use std::time::SystemTime;

    struct Fixture {
        job: Option<RegisteredJob>,
        root: PathBuf,
    }
    impl Fixture {
        fn new() -> Self {
            let mut random = [0; 16];
            getrandom::fill(&mut random).unwrap();
            let root = Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("target/input-reader-fixtures")
                .join(format!("{:032x}", u128::from_le_bytes(random)));
            fs::create_dir_all(root.join(".cpforge")).unwrap();
            let root = fs::canonicalize(root).unwrap();
            fs::write(
                root.join(".cpforge/job-seq"),
                "schema = 1\nepoch = '0123456789abcdef0123456789abcdef'\nnext = '0'\n",
            )
            .unwrap();
            let lock = WorkspaceLock::new(&root).unwrap();
            let job = register_job(
                lock.acquire("input reader test").unwrap(),
                PrunePolicy::default(),
                4096,
                SystemTime::now(),
            )
            .unwrap();
            Self {
                job: Some(job),
                root,
            }
        }
        fn job(&self) -> &RegisteredJob {
            self.job.as_ref().unwrap()
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            drop(self.job.take());
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    struct GrowingReader {
        original: std::io::Cursor<Vec<u8>>,
        extra: std::io::Cursor<Vec<u8>>,
    }
    impl Read for GrowingReader {
        fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
            let count = self.original.read(buffer)?;
            if count != 0 {
                return Ok(count);
            }
            self.extra.read(buffer)
        }
    }

    #[test]
    fn growth_beyond_initial_length_reserves_each_chunk_before_writing() {
        let fixture = Fixture::new();
        let job = fixture.job();
        let before = job.budget.usage().retained;
        // Leave five bytes available, even though the observed original length
        // is only three. The reader later supplies four additional bytes.
        let held = job.budget.reserve(job.budget.limit() - before - 5).unwrap();
        let mut reader = GrowingReader {
            original: std::io::Cursor::new(b"abc".to_vec()),
            extra: std::io::Cursor::new(b"defg".to_vec()),
        };
        let error = job
            .inputs()
            .copy(Path::new("growing.in"), &mut reader)
            .unwrap_err();
        assert!(matches!(
            error,
            InputsError::Budget(BudgetExceeded { requested: 4, .. })
        ));
        assert_eq!(fs::read(job.dir.join("inputs/growing.in")).unwrap(), b"abc");
        assert_eq!(job.budget.usage().retained, before + 3);
        assert_eq!(job.budget.usage().reserved, held.bytes());
        assert!(job.budget.usage().retained + job.budget.usage().reserved <= job.budget.limit());
        drop(held);
        assert_eq!(job.budget.usage().reserved, 0);
        assert!(matches!(
            job.inputs().copy(Path::new("retry.in"), &mut &b""[..]),
            Err(InputsError::Failed)
        ));
    }

    #[test]
    fn growth_within_budget_copies_and_hashes_all_bytes() {
        let fixture = Fixture::new();
        let job = fixture.job();
        let before = job.budget.usage().retained;
        let mut reader = GrowingReader {
            original: std::io::Cursor::new(b"abc".to_vec()),
            extra: std::io::Cursor::new(b"defg".to_vec()),
        };
        let copy = job
            .inputs()
            .copy(Path::new("growing.in"), &mut reader)
            .unwrap();
        assert_eq!(copy.len, 7);
        assert_eq!(copy.sha256, <[u8; 32]>::from(Sha256::digest(b"abcdefg")));
        assert_eq!(fs::read(copy.path).unwrap(), b"abcdefg");
        assert_eq!(job.budget.usage().retained, before + 7);
        assert_eq!(job.budget.usage().reserved, 0);
    }

    #[test]
    fn read_failure_keeps_partial_bytes_accounted_and_returns_no_copy() {
        struct FailingReader(bool);
        impl Read for FailingReader {
            fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
                if self.0 {
                    return Err(io::Error::other("injected read failure"));
                }
                self.0 = true;
                buffer[..3].copy_from_slice(b"abc");
                Ok(3)
            }
        }
        let fixture = Fixture::new();
        let job = fixture.job();
        let before = job.budget.usage().retained;
        assert!(
            job.inputs()
                .copy(Path::new("partial.in"), &mut FailingReader(false))
                .is_err()
        );
        assert_eq!(fs::read(job.dir.join("inputs/partial.in")).unwrap(), b"abc");
        assert_eq!(job.budget.usage().retained, before + 3);
        assert_eq!(job.budget.usage().reserved, 0);
    }

    #[test]
    fn capability_refuses_paths_outside_inputs() {
        for path in [PathBuf::from("../outside"), PathBuf::from("/outside")] {
            let fixture = Fixture::new();
            let job = fixture.job();
            assert!(
                matches!(job.inputs().copy(&path, &mut &b"data"[..]), Err(InputsError::Storage(error)) if matches!(error.kind, ErrorKind::InvalidPath { .. }))
            );
            assert!(!job.dir.join("outside").exists());
        }
        let fixture = Fixture::new();
        let job = fixture.job();
        // Even an absolute path inside the job cannot bypass the inputs root.
        assert!(
            job.inputs()
                .copy(&job.dir.join("outside"), &mut &b"data"[..])
                .is_err()
        );
        assert!(!job.dir.join("outside").exists());
    }
}
