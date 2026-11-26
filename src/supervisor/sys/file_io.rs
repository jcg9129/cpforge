//! Directory-relative file-I/O operations. No operation follows a solution link.
use std::ffi::{CStr, CString, OsStr};
use std::fs::{File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, IntoRawFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::report::{CollectionFailure, IoScratchDetail, OutputFile, OutputFileType};
use sha2::{Digest, Sha256};

pub(crate) struct Directory {
    file: File,
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
        OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(path)
            .map(|file| Self {
                file,
                #[cfg(test)]
                operations: Default::default(),
                #[cfg(test)]
                before_listing: Default::default(),
            })
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
    pub(crate) fn stage(&self, name: &OsStr, source: &Path, bytes: u64) -> Result<(), StageError> {
        let component = CString::new(name.as_bytes()).map_err(|e| StageError {
            error: e.into(),
            kept: false,
        })?;
        let output = self
            .open_at(&component, libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL)
            .map_err(|error| StageError { error, kept: false })?;
        fill(output, source, bytes).map_err(|error| StageError {
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
        #[cfg(test)]
        if let Some(hook) = self.before_listing.borrow_mut().take() {
            hook();
        }
        self.observe("listing", c".", 0);
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
        let mut result = Listing::default();
        let key = crate::names::collision_key(name.to_str().expect("validated name"));
        let mut count = 0;
        loop {
            // SAFETY: DIR is exclusively owned and the entry is consumed before the next call.
            let entry = unsafe {
                set_errno(0);
                libc::readdir(dir.0)
            };
            if entry.is_null() {
                let error = io::Error::last_os_error();
                return if error.raw_os_error() == Some(0) {
                    Ok(result)
                } else {
                    Err(error)
                };
            }
            // SAFETY: a non-null entry stays valid until the next readdir on this stream,
            // and d_name is NUL-terminated.
            let entry = unsafe { &*entry };
            let bytes = unsafe { CStr::from_ptr(entry.d_name.as_ptr()) }.to_bytes();
            if bytes == b"." || bytes == b".." {
                continue;
            }
            count += 1;
            if count > 65_536 {
                result.cutoff = true;
                return Ok(result);
            }
            if bytes == name.as_bytes() {
                result.inode = Some(entry.d_ino as u64);
                return Ok(result);
            }
            if let Ok(text) = std::str::from_utf8(bytes) {
                result.alias |= crate::names::collision_key(text) == key;
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
            let Some(inode) = listing.inode else {
                return if listing.cutoff {
                    OutputFile::TooManyEntries
                } else if listing.alias {
                    OutputFile::Misnamed
                } else {
                    OutputFile::Missing
                };
            };
            let stat = match observation {
                Ok(stat) if stat.st_ino as u64 == inode => stat,
                Ok(_) => continue,
                Err(e)
                    if matches!(
                        e.raw_os_error(),
                        Some(libc::ENOENT | libc::ELOOP | libc::ENXIO)
                    ) =>
                {
                    continue;
                }
                Err(e) => return failure(e),
            };
            let file_type = match stat.st_mode & libc::S_IFMT {
                libc::S_IFREG => None,
                libc::S_IFLNK => Some(OutputFileType::Link),
                libc::S_IFDIR => Some(OutputFileType::Directory),
                _ => Some(OutputFileType::Other),
            };
            if let Some(file_type) = file_type {
                return OutputFile::NotRegular { file_type };
            }
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
            return extract(file, budget, destination, retained, warnings);
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

#[derive(Default)]
struct Listing {
    inode: Option<u64>,
    alias: bool,
    cutoff: bool,
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
    if e.raw_os_error() == Some(libc::EACCES) {
        OutputFile::Unreadable {
            os_error: e.raw_os_error().expect("permission errno"),
        }
    } else {
        io_failure(e)
    }
}
fn io_failure(e: io::Error) -> OutputFile {
    OutputFile::NotCollected {
        reason: CollectionFailure::Io,
        os_error: e.raw_os_error(),
    }
}

fn fill(mut output: File, source: &Path, bytes: u64) -> io::Result<()> {
    let mut input = File::open(source)?.take(bytes);
    if io::copy(&mut input, &mut output)? != bytes {
        return Err(io::ErrorKind::UnexpectedEof.into());
    }
    output.flush()?;
    close(output)
}

fn close(file: File) -> io::Result<()> {
    // SAFETY: ownership of this fd was moved out of File, and it is closed exactly once.
    if unsafe { libc::close(file.into_raw_fd()) } == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

fn extract(
    source: File,
    budget: u64,
    destination: &Path,
    retained: &AtomicU64,
    warnings: &mut Vec<IoScratchDetail>,
) -> OutputFile {
    let mut copy = BoundedCopy {
        written: 0,
        created: false,
    };
    let outcome = copy
        .run(source, budget, destination, retained)
        .unwrap_or_else(io_failure);
    if copy.created && !matches!(outcome, OutputFile::Collected { .. }) {
        match std::fs::remove_file(destination) {
            Ok(()) => {
                retained.fetch_sub(copy.written, Ordering::AcqRel);
            }
            Err(_) => warnings.push(IoScratchDetail::PartialCopyKept),
        }
    }
    outcome
}

/// What a bounded copy left on disk, also after an error.
struct BoundedCopy {
    written: u64,
    created: bool,
}
impl BoundedCopy {
    fn run(
        &mut self,
        mut source: File,
        budget: u64,
        destination: &Path,
        retained: &AtomicU64,
    ) -> io::Result<OutputFile> {
        let mut output = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(destination)?;
        self.created = true;
        let mut hash = Sha256::new();
        let mut buffer = [0; 64 * 1024];
        loop {
            let len = (budget - self.written).min(buffer.len() as u64) as usize;
            // At the cap, probe into memory only; the retained file never exceeds its reservation.
            let n = loop {
                match source.read(&mut buffer[..len.max(1)]) {
                    Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                    result => break result?,
                }
            };
            if n == 0 {
                break;
            }
            if len == 0 {
                return Ok(OutputFile::TooLarge {
                    bytes: budget + 1,
                    budget,
                });
            }
            let mut pending = &buffer[..n];
            while !pending.is_empty() {
                let n = match output.write(pending) {
                    Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                    result => result?,
                };
                if n == 0 {
                    return Err(io::ErrorKind::WriteZero.into());
                }
                hash.update(&pending[..n]);
                self.written += n as u64;
                retained.fetch_add(n as u64, Ordering::AcqRel);
                pending = &pending[n..];
            }
        }
        output.flush()?;
        close(output)?;
        let run = destination
            .parent()
            .and_then(Path::parent)
            .and_then(Path::file_name)
            .expect("run");
        Ok(OutputFile::Collected {
            bytes: self.written,
            sha256: hash.finalize().into(),
            path: format!("{}/out/output", run.to_string_lossy()),
        })
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
        std::fs::set_permissions(f.0.join("out.txt"), std::fs::Permissions::from_mode(0)).unwrap();
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
