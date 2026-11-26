//! Windows unit-test adapter. Identity and spelling are synthetic; lifecycle,
//! collection decisions, extraction, faults and accounting are production code.
use crate::report::{CollectionFailure, IoScratchDetail, OutputFile, OutputFileType};
use crate::supervisor::file_io_copy::extract;
use crate::supervisor::file_io_hooks::{DirectoryCount, Hooks};
use crate::supervisor::file_io_listing::{Decision, Listing, Observation, Scan, decide};
use std::ffi::OsStr;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicU64;

/// The stub has no inode numbers: every observation and listing agrees.
const INODE: u64 = 42;

#[derive(Debug)]
pub(crate) struct Directory {
    path: PathBuf,
    hooks: Hooks,
    _count: DirectoryCount,
}
#[derive(Debug)]
pub(crate) struct StageError {
    pub(crate) error: io::Error,
    pub(crate) kept: bool,
}
enum Attempt {
    Decided(OutputFile),
    Retry,
    Accepted(File),
}
impl Directory {
    pub(crate) fn open(path: &Path) -> io::Result<Self> {
        let hooks = Hooks::environment();
        hooks.at("file_io.directory", 0)?;
        Ok(Self {
            path: path.to_owned(),
            hooks,
            _count: DirectoryCount::new(),
        })
    }
    pub(crate) fn observed(
        mut self,
        observer: Option<crate::jobs::engine::Observation>,
        execution: u32,
    ) -> Self {
        self.hooks.observation = observer.map(|o| (o, execution));
        self
    }
    pub(crate) fn set_hooks(&mut self, mut hooks: Hooks) {
        hooks.observation = self.hooks.observation.take();
        self.hooks = hooks;
    }
    pub(crate) fn matches(&self, path: &Path) -> bool {
        self.path == path
    }
    pub(crate) fn accounted(
        mut self,
        budget: Option<std::sync::Arc<crate::storage::budget::JobBudget>>,
    ) -> Self {
        self.hooks.budget = budget;
        self
    }

    pub(crate) fn remove_copy(&self, path: &Path) -> io::Result<()> {
        self.hooks.at("file_io.partial.unlink", 0)?;
        fs::remove_file(path)
    }
    pub(crate) fn remove(&self, name: &OsStr, only_file: bool) -> io::Result<()> {
        let path = self.path.join(name);
        if only_file {
            self.hooks.at("file_io.type_check", 0)?;
            match fs::symlink_metadata(&path) {
                Ok(m) if !(m.is_file() || m.is_symlink()) => return Ok(()),
                Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
                Err(e) => return Err(e),
                _ => {}
            }
        }
        self.hooks.at("file_io.unlink", 0)?;
        match fs::remove_file(path) {
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
            result => result,
        }
    }
    pub(crate) fn stage(&self, name: &OsStr, source: &Path, bytes: u64) -> Result<(), StageError> {
        self.hooks
            .at("file_io.open", 0)
            .map_err(|error| StageError { error, kept: false })?;
        let file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(self.path.join(name))
            .map_err(|error| StageError { error, kept: false })?;
        self.fill(file, source, bytes).map_err(|error| StageError {
            error,
            kept: self.remove(name, true).is_err(),
        })
    }
    fn fill(&self, mut file: File, source: &Path, bytes: u64) -> io::Result<()> {
        let size = usize::try_from(bytes).unwrap_or(usize::MAX);
        self.hooks.at("file_io.staging.read", size)?;
        let mut input = File::open(source)?.take(bytes);
        self.hooks.at("file_io.staging.write", size)?;
        if io::copy(&mut input, &mut file)? != bytes {
            return Err(io::ErrorKind::UnexpectedEof.into());
        }
        file.flush()
    }

    /// The same attempt structure and decision as the native collector:
    /// observation, an open only of a regular file, the confirming listing,
    /// then `decide`; at most three attempts.
    pub(crate) fn collect(
        &self,
        name: &OsStr,
        budget: u64,
        destination: &Path,
        retained: &AtomicU64,
        warnings: &mut Vec<IoScratchDetail>,
    ) -> OutputFile {
        for _ in 0..3 {
            match self.attempt(name, budget) {
                Attempt::Decided(outcome) => return outcome,
                Attempt::Retry => {}
                Attempt::Accepted(file) => {
                    return extract(file, budget, destination, retained, warnings, &self.hooks);
                }
            }
        }
        OutputFile::NotCollected {
            reason: CollectionFailure::Unstable,
            os_error: None,
        }
    }
    fn attempt(&self, name: &OsStr, budget: u64) -> Attempt {
        let path = self.path.join(name);
        let observation = self
            .hooks
            .at("file_io.type_check", 0)
            .and_then(|()| fs::symlink_metadata(&path));
        let opened = match &observation {
            Ok(metadata) if metadata.is_file() => Some(
                self.hooks
                    .at("file_io.open", 0)
                    .and_then(|()| File::open(&path))
                    .and_then(|file| file.metadata().map(|metadata| (file, metadata))),
            ),
            _ => None,
        };
        let listing = match self.listing(name) {
            Ok(listing) => listing,
            Err(e) => return Attempt::Decided(failure(&e)),
        };
        if let Err(e) = self.hooks.at("file_io.after_listing", 0) {
            return Attempt::Decided(failure(&e));
        }
        let observed = match &observation {
            Ok(metadata) => Observation::Entry {
                inode: INODE,
                kind: if metadata.is_file() {
                    None
                } else if metadata.is_symlink() {
                    Some(OutputFileType::Link)
                } else if metadata.is_dir() {
                    Some(OutputFileType::Directory)
                } else {
                    Some(OutputFileType::Other)
                },
            },
            Err(e) if e.kind() == io::ErrorKind::NotFound => Observation::Retry,
            Err(e) => Observation::Failed(failure(e)),
        };
        match decide(listing, observed) {
            Decision::Outcome(outcome) => return Attempt::Decided(outcome),
            Decision::Retry => return Attempt::Retry,
            Decision::Accept => {}
        }
        let (file, metadata) = match opened.expect("regular observation") {
            Ok(opened) => opened,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Attempt::Retry,
            Err(e) => return Attempt::Decided(failure(&e)),
        };
        if metadata.len() > budget {
            return Attempt::Decided(OutputFile::TooLarge {
                bytes: metadata.len(),
                budget,
            });
        }
        Attempt::Accepted(file)
    }
    fn listing(&self, name: &OsStr) -> io::Result<Listing> {
        self.hooks.at("file_io.before_listing", 0)?;
        self.hooks.at("file_io.listing", 0)?;
        let mut scan = Scan::new(name);
        if let Some(position) = self.hooks.listing_position {
            for index in 1..=position {
                self.hooks.at("file_io.listing.entry", 0)?;
                let bytes = if index == position {
                    name.as_encoded_bytes()
                } else {
                    b"stub-entry"
                };
                if scan.entry(bytes, INODE) {
                    break;
                }
            }
        } else {
            for entry in fs::read_dir(&self.path)? {
                self.hooks.at("file_io.listing.entry", 0)?;
                if scan.entry(entry?.file_name().as_encoded_bytes(), INODE) {
                    break;
                }
            }
        }
        Ok(scan.finish())
    }
}

fn failure(e: &io::Error) -> OutputFile {
    match e.raw_os_error() {
        Some(os_error) if e.kind() == io::ErrorKind::PermissionDenied => {
            OutputFile::Unreadable { os_error }
        }
        os_error => OutputFile::NotCollected {
            reason: CollectionFailure::Io,
            os_error,
        },
    }
}
