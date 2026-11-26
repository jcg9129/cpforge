//! Directory-relative file-I/O operations. No operation follows a solution link.
use std::ffi::{CStr, CString, OsStr};
use std::fs::{File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, IntoRawFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::Path;
use std::sync::atomic::AtomicU64;

use crate::report::{CollectionFailure, IoScratchDetail, OutputFile, OutputFileType};
use crate::supervisor::file_io_copy::extract;
use crate::supervisor::file_io_listing::{Decision, Listing, Observation, Scan, decide};

pub(crate) struct Directory {
    file: File,
    #[cfg(feature = "storage-test-hooks")]
    hooks: crate::supervisor::file_io_hooks::Hooks,
    #[cfg(feature = "storage-test-hooks")]
    _directory_count: crate::supervisor::file_io_hooks::DirectoryCount,
    #[cfg(test)]
    operations: std::cell::RefCell<Vec<(&'static str, Vec<u8>, i32)>>,
    #[cfg(test)]
    before_listing: std::cell::RefCell<Option<Box<dyn FnOnce() + Send>>>,
}
impl std::fmt::Debug for Directory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_tuple("Directory").field(&self.file).finish()
    }
}
impl Directory {
    pub(crate) fn open(path: &Path) -> io::Result<Self> {
        #[cfg(feature = "storage-test-hooks")]
        let hooks = crate::supervisor::file_io_hooks::Hooks::environment();
        #[cfg(feature = "storage-test-hooks")]
        hooks.at("file_io.directory", 0)?;
        OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(path)
            .map(|file| Self {
                file,
                #[cfg(feature = "storage-test-hooks")]
                hooks,
                #[cfg(feature = "storage-test-hooks")]
                _directory_count: crate::supervisor::file_io_hooks::DirectoryCount::new(),
                #[cfg(test)]
                operations: Default::default(),
                #[cfg(test)]
                before_listing: Default::default(),
            })
    }

    #[cfg(feature = "storage-test-hooks")]
    pub(crate) fn observed(
        mut self,
        observer: Option<crate::jobs::engine::Observation>,
        execution: u32,
    ) -> Self {
        self.hooks.observation = observer.map(|observer| (observer, execution));
        self
    }

    #[cfg(all(test, feature = "storage-test-hooks"))]
    pub(crate) fn set_hooks(&mut self, mut hooks: crate::supervisor::file_io_hooks::Hooks) {
        hooks.observation = self.hooks.observation.take();
        self.hooks = hooks;
    }

    #[cfg(feature = "storage-test-hooks")]
    pub(crate) fn accounted(
        mut self,
        budget: Option<std::sync::Arc<crate::storage::budget::JobBudget>>,
    ) -> Self {
        self.hooks.budget = budget;
        self
    }

    pub(crate) fn remove_copy(&self, destination: &Path) -> io::Result<()> {
        #[cfg(feature = "storage-test-hooks")]
        self.hooks.at("file_io.partial.unlink", 0)?;
        std::fs::remove_file(destination)
    }

    // The operation log and between-observation/listing hook are local to H;
    // later fault fixtures can replace individual operations without global state.
    fn observe(&self, call: &'static str, name: &CStr, flags: i32) {
        #[cfg(test)]
        self.operations
            .borrow_mut()
            .push((call, name.to_bytes().to_vec(), flags));
        #[cfg(not(test))]
        let _ = (call, name, flags);
    }

    fn stat(&self, name: &CStr) -> io::Result<libc::stat> {
        #[cfg(feature = "storage-test-hooks")]
        self.hooks.at("file_io.type_check", 0)?;
        self.observe("fstatat", name, libc::AT_SYMLINK_NOFOLLOW);
        // SAFETY: stat is initialized by a successful fstatat, and both arguments live.
        unsafe {
            let mut stat = std::mem::zeroed();
            if libc::fstatat(
                self.file.as_raw_fd(),
                name.as_ptr(),
                &mut stat,
                libc::AT_SYMLINK_NOFOLLOW,
            ) != 0
            {
                return Err(io::Error::last_os_error());
            }
            Ok(stat)
        }
    }

    fn open_at(&self, name: &CStr, flags: i32) -> io::Result<File> {
        #[cfg(feature = "storage-test-hooks")]
        self.hooks.at("file_io.open", 0)?;
        self.observe("openat", name, flags | libc::O_CLOEXEC | libc::O_NOFOLLOW);
        // SAFETY: H and the terminated component live; ownership of a successful fd moves to File.
        let fd = unsafe {
            libc::openat(
                self.file.as_raw_fd(),
                name.as_ptr(),
                flags | libc::O_CLOEXEC | libc::O_NOFOLLOW,
                0o644,
            )
        };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: fd is a freshly opened descriptor owned by nothing else.
        Ok(unsafe { File::from_raw_fd(fd) })
    }

    /// Creates `name` exclusively and fills it with exactly `bytes` bytes of
    /// `source`. On failure after creation the partial file is removed here;
    /// the error then says whether a file is still present at the name (only
    /// when that removal failed), so a pre-existing entry is never removed.
    pub(in crate::supervisor) fn stage(
        &self,
        name: &OsStr,
        source: &Path,
        bytes: u64,
    ) -> Result<(), StageError> {
        let component = CString::new(name.as_bytes()).map_err(|e| StageError {
            error: e.into(),
            kept: false,
        })?;
        let output = self
            .open_at(&component, libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL)
            .map_err(|error| StageError { error, kept: false })?;
        fill(
            output,
            source,
            bytes,
            #[cfg(feature = "storage-test-hooks")]
            &self.hooks,
        )
        .map_err(|error| StageError {
            error,
            kept: self.remove(name, true).is_err(),
        })
    }

    pub(crate) fn remove(&self, name: &OsStr, only_file: bool) -> io::Result<()> {
        let name = CString::new(name.as_bytes())?;
        if only_file {
            match self.stat(&name) {
                Ok(stat)
                    if !matches!(stat.st_mode & libc::S_IFMT, libc::S_IFREG | libc::S_IFLNK) =>
                {
                    return Ok(());
                }
                Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
                Err(e) => return Err(e),
                _ => {}
            }
        }
        #[cfg(feature = "storage-test-hooks")]
        self.hooks.at("file_io.unlink", 0)?;
        self.observe("unlinkat", &name, 0);
        // SAFETY: unlinkat addresses the entry through live H and never follows a link.
        if unsafe { libc::unlinkat(self.file.as_raw_fd(), name.as_ptr(), 0) } == 0 {
            return Ok(());
        }
        let error = io::Error::last_os_error();
        if error.kind() == io::ErrorKind::NotFound {
            Ok(())
        } else {
            Err(error)
        }
    }

    pub(crate) fn matches(&self, path: &Path) -> bool {
        let Ok(actual) = std::fs::symlink_metadata(path) else {
            return false;
        };
        let Ok(held) = self.file.metadata() else {
            return false;
        };
        actual.is_dir() && actual.dev() == held.dev() && actual.ino() == held.ino()
    }

    // One new directory description per attempt (dup alone shares the directory offset).
    // Keep this bounded listing separate from observation and open for deterministic injection.
    #[allow(clippy::unnecessary_cast)] // libc inode widths differ across Unix targets.
    fn listing(&self, name: &OsStr) -> io::Result<Listing> {
        #[cfg(feature = "storage-test-hooks")]
        self.hooks.at("file_io.before_listing", 0)?;
        #[cfg(test)]
        if let Some(hook) = self.before_listing.borrow_mut().take() {
            hook();
        }
        self.observe("listing", c".", 0);
        #[cfg(feature = "storage-test-hooks")]
        {
            self.hooks.at("file_io.listing", 0)?;
            if let Some(position) = self.hooks.listing_position {
                self.hooks.at("file_io.listing.entry", 0)?;
                let component = CString::new(name.as_bytes())?;
                let inode = self.stat(&component)?.st_ino as u64;
                let mut scan = Scan::new(name);
                for index in 1..=position {
                    let bytes = if index == position {
                        name.as_bytes()
                    } else {
                        b"stub-entry"
                    };
                    if scan.entry(bytes, inode) {
                        break;
                    }
                }
                return Ok(scan.finish());
            }
        }
        let fd = self.open_at(c".", libc::O_RDONLY | libc::O_DIRECTORY)?;
        let fd = fd.into_raw_fd();
        // SAFETY: fd is exclusively owned here and handed to DIR on success.
        let dir = unsafe { libc::fdopendir(fd) };
        if dir.is_null() {
            let error = io::Error::last_os_error();
            // SAFETY: fdopendir failed, so fd is still exclusively owned here.
            unsafe {
                libc::close(fd);
            }
            return Err(error);
        }
        struct Entries(*mut libc::DIR);
        impl Drop for Entries {
            fn drop(&mut self) {
                // SAFETY: the stream came from fdopendir and is closed once.
                unsafe {
                    libc::closedir(self.0);
                }
            }
        }
        let dir = Entries(dir);
        let mut scan = Scan::new(name);
        loop {
            #[cfg(feature = "storage-test-hooks")]
            self.hooks.at("file_io.listing.entry", 0)?;
            // SAFETY: DIR is exclusively owned and the entry is consumed before the next call.
            let entry = unsafe {
                set_errno(0);
                libc::readdir(dir.0)
            };
            if entry.is_null() {
                let error = io::Error::last_os_error();
                return if error.raw_os_error() == Some(0) {
                    Ok(scan.finish())
                } else {
                    Err(error)
                };
            }
            // SAFETY: a non-null entry stays valid until the next readdir on this stream,
            // and d_name is NUL-terminated.
            let entry = unsafe { &*entry };
            let bytes = unsafe { CStr::from_ptr(entry.d_name.as_ptr()) }.to_bytes();
            if scan.entry(bytes, entry.d_ino as u64) {
                return Ok(scan.finish());
            }
        }
    }

    #[allow(clippy::unnecessary_cast)] // libc device and inode widths differ across Unix targets.
    pub(crate) fn collect(
        &self,
        name: &OsStr,
        budget: u64,
        destination: &Path,
        retained: &AtomicU64,
        warnings: &mut Vec<IoScratchDetail>,
    ) -> OutputFile {
        for _ in 0..3 {
            let component = CString::new(name.as_bytes()).expect("validated component");
            let observation = self.stat(&component);
            let opened = observation
                .as_ref()
                .ok()
                .filter(|s| s.st_mode & libc::S_IFMT == libc::S_IFREG)
                .map(|_| {
                    self.open_at(&component, libc::O_RDONLY | libc::O_NONBLOCK)
                        .and_then(|file| file.metadata().map(|metadata| (file, metadata)))
                });
            let listing = match self.listing(name) {
                Ok(l) => l,
                Err(e) => return failure(e),
            };
            #[cfg(feature = "storage-test-hooks")]
            if let Err(e) = self.hooks.at("file_io.after_listing", 0) {
                return failure(e);
            }
            let observed = match &observation {
                Ok(stat) => Observation::Entry {
                    inode: stat.st_ino as u64,
                    kind: match stat.st_mode & libc::S_IFMT {
                        libc::S_IFREG => None,
                        libc::S_IFLNK => Some(OutputFileType::Link),
                        libc::S_IFDIR => Some(OutputFileType::Directory),
                        _ => Some(OutputFileType::Other),
                    },
                },
                Err(e)
                    if matches!(
                        e.raw_os_error(),
                        Some(libc::ENOENT | libc::ELOOP | libc::ENXIO)
                    ) =>
                {
                    Observation::Retry
                }
                Err(e) => Observation::Failed(observation_failure(e)),
            };
            match decide(listing, observed) {
                Decision::Outcome(outcome) => return outcome,
                Decision::Retry => continue,
                Decision::Accept => {}
            }
            let stat = observation.expect("accepted regular observation");
            let inode = stat.st_ino as u64;
            let (file, metadata) = match opened.expect("regular observation") {
                Ok(f) => f,
                Err(e)
                    if matches!(
                        e.raw_os_error(),
                        Some(libc::ENOENT | libc::ELOOP | libc::ENXIO)
                    ) =>
                {
                    continue;
                }
                Err(e) if matches!(e.raw_os_error(), Some(libc::EACCES | libc::EPERM)) => {
                    return OutputFile::Unreadable {
                        os_error: e.raw_os_error().expect("permission errno"),
                    };
                }
                Err(e) => return io_failure(e),
            };
            if !metadata.is_file()
                || metadata.dev() != stat.st_dev as u64
                || metadata.ino() != inode
            {
                continue;
            }
            if metadata.len() > budget {
                return OutputFile::TooLarge {
                    bytes: metadata.len(),
                    budget,
                };
            }
            return extract(
                file,
                budget,
                destination,
                retained,
                warnings,
                #[cfg(feature = "storage-test-hooks")]
                &self.hooks,
            );
        }
        OutputFile::NotCollected {
            reason: CollectionFailure::Unstable,
            os_error: None,
        }
    }
}
/// A failed staging; `kept` is true only when a partial file could not be removed.
#[derive(Debug)]
pub(crate) struct StageError {
    pub(crate) error: io::Error,
    pub(crate) kept: bool,
}

#[cfg(target_os = "linux")]
unsafe fn set_errno(value: i32) {
    unsafe {
        *libc::__errno_location() = value;
    }
}
#[cfg(target_os = "macos")]
unsafe fn set_errno(value: i32) {
    unsafe {
        *libc::__error() = value;
    }
}

fn failure(e: io::Error) -> OutputFile {
    observation_failure(&e)
}
fn observation_failure(e: &io::Error) -> OutputFile {
    if e.raw_os_error() == Some(libc::EACCES) {
        OutputFile::Unreadable {
            os_error: e.raw_os_error().expect("permission errno"),
        }
    } else {
        OutputFile::NotCollected {
            reason: CollectionFailure::Io,
            os_error: e.raw_os_error(),
        }
    }
}
fn io_failure(e: io::Error) -> OutputFile {
    OutputFile::NotCollected {
        reason: CollectionFailure::Io,
        os_error: e.raw_os_error(),
    }
}

fn fill(
    mut output: File,
    source: &Path,
    bytes: u64,
    #[cfg(feature = "storage-test-hooks")] hooks: &crate::supervisor::file_io_hooks::Hooks,
) -> io::Result<()> {
    #[cfg(feature = "storage-test-hooks")]
    hooks.at(
        "file_io.staging.read",
        bytes.min(usize::MAX as u64) as usize,
    )?;
    let mut input = File::open(source)?.take(bytes);
    #[cfg(feature = "storage-test-hooks")]
    {
        hooks.at(
            "file_io.staging.write",
            bytes.min(usize::MAX as u64) as usize,
        )?;
    }
    if io::copy(&mut input, &mut output)? != bytes {
        return Err(io::ErrorKind::UnexpectedEof.into());
    }
    output.flush()?;
    close(output)
}

pub(crate) fn close(file: File) -> io::Result<()> {
    // SAFETY: ownership of this fd was moved out of File, and it is closed exactly once.
    if unsafe { libc::close(file.into_raw_fd()) } == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::{PermissionsExt, symlink};

    struct Fixture(std::path::PathBuf);
    impl Fixture {
        fn new() -> Self {
            let path = Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("target")
                .join(format!(
                    "file-ops-{}",
                    crate::build::BuildId::generate().unwrap()
                ));
            std::fs::create_dir_all(&path).unwrap();
            Self(path)
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    // F-10's syscall evidence supplements the real-command outcome assertions.
    #[test]
    fn links_are_observed_and_listed_without_being_opened() {
        for target in ["answer", "/etc/hosts", "pipe"] {
            let f = Fixture::new();
            std::fs::write(f.0.join("answer"), b"42\n").unwrap();
            let pipe = CString::new(f.0.join("pipe").as_os_str().as_bytes()).unwrap();
            assert_eq!(unsafe { libc::mkfifo(pipe.as_ptr(), 0o600) }, 0);
            symlink(target, f.0.join("out.txt")).unwrap();
            let directory = Directory::open(&f.0).unwrap();
            let outcome = directory.collect(
                OsStr::new("out.txt"),
                4096,
                &f.0.join("extracted"),
                &AtomicU64::new(0),
                &mut Vec::new(),
            );
            assert_eq!(
                outcome,
                OutputFile::NotRegular {
                    file_type: OutputFileType::Link
                }
            );
            let operations = directory.operations.borrow();
            assert!(
                operations
                    .iter()
                    .any(|(call, name, flags)| *call == "fstatat"
                        && name == b"out.txt"
                        && *flags == libc::AT_SYMLINK_NOFOLLOW)
            );
            assert!(operations.iter().any(|(call, _, _)| *call == "listing"));
            assert!(
                operations
                    .iter()
                    .filter(|(call, _, _)| *call == "openat")
                    .all(|(_, name, _)| name == b".")
            );
        }
    }

    // F-34: the spelling decision also outranks an earlier EACCES/open result.
    #[test]
    fn unreadable_file_renamed_before_listing_is_misnamed() {
        let f = Fixture::new();
        std::fs::write(f.0.join("out.txt"), b"42\n").unwrap();
        std::fs::set_permissions(f.0.join("out.txt"), std::fs::Permissions::from_mode(0o000))
            .unwrap();
        let directory = Directory::open(&f.0).unwrap();
        let path = f.0.clone();
        *directory.before_listing.borrow_mut() = Some(Box::new(move || {
            std::fs::rename(path.join("out.txt"), path.join("OUT.TXT")).unwrap();
        }));
        let outcome = directory.collect(
            OsStr::new("out.txt"),
            4096,
            &f.0.join("extracted"),
            &AtomicU64::new(0),
            &mut Vec::new(),
        );
        assert_eq!(outcome, OutputFile::Misnamed);
        assert!(!f.0.join("extracted").exists());
    }
}
