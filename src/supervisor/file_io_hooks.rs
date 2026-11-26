//! Feature-only file-I/O faults, synthetic reads/listings and barriers.
//!
//! `CPF_FILE_IO_FAULTS` is a comma-separated list `point=ERRNO[@occurrence]`.
//! Errnos may be native numbers or EIO, ENOSPC, EACCES, EPERM, EINTR, ENOENT.
//! Without an occurrence the fault applies on every call. Points are:
//! `file_io.directory`, `file_io.type_check`, `file_io.open`,
//! `file_io.listing`, `file_io.listing.entry`, `file_io.read`,
//! `file_io.destination`, `file_io.write`, `file_io.staging.read`,
//! `file_io.staging.write`, `file_io.unlink`, `file_io.partial.unlink`.
//! Every point also supports the existing CPF_STORAGE_PAUSE_AT barriers.
//! `file_io.before_listing`, `file_io.after_listing`, `file_io.extracted`
//! are observation barriers only; so are `file_io.copied` (size: bytes
//! written to the copy so far), `file_io.copy_length` (size: the copy's
//! on-disk length) and `file_io.probe` (before the 1-byte read into memory
//! at the cap). The barrier directory protocol permits an
//! external driver to cancel the job while a worker remains blocked.
//!
//! `CPF_FILE_IO_READ_SIZE` replaces the accepted handle's read stream with
//! that many `x` bytes (independent of fstat); `CPF_FILE_IO_READ_CHUNK` bounds
//! each read. `CPF_FILE_IO_LISTING_POSITION` stubs the exact name's position,
//! retaining the real inode. `CPF_FILE_IO_LOG` appends point, per-point call
//! count, requested size and thread identity; once the directory knows the
//! job's budget, each point also appends `file_io.charged` (retained plus
//! reserved bytes) and `file_io.limit` lines. No hook state exists in a
//! feature-disabled build. Settings are captured per directory, so inline
//! tests can supply local Hooks without changing the process environment.
//! `CPF_FILE_IO_UNVERIFIED` marks the cleanup result unverified before
//! collection (the OS cleanup itself still runs). Engine observers receive
//! the same boundaries as Point::FileIo, so a driver can publish cancellation
//! while `file_io.staging.write` or a collection operation remains blocked.
use std::collections::HashMap;
use std::io::{self, Read, Write};
use std::path::PathBuf;
use std::sync::Mutex;

#[derive(Debug, Default)]
pub(crate) struct Hooks {
    pub(crate) observation: Option<(crate::jobs::engine::Observation, u32)>,
    pub(crate) faults: String,
    pub(crate) budget: Option<std::sync::Arc<crate::storage::budget::JobBudget>>,
    pub(crate) read_size: Option<u64>,
    pub(crate) read_chunk: Option<usize>,
    pub(crate) listing_position: Option<u64>,
    log: Option<PathBuf>,
    calls: Mutex<HashMap<&'static str, u64>>,
    read: Mutex<u64>,
}
impl Hooks {
    #[cfg(test)]
    pub(crate) fn testing(faults: &str, read_size: Option<u64>) -> Self {
        Self {
            faults: faults.into(),
            read_size,
            read_chunk: Some(1024),
            ..Self::default()
        }
    }

    pub(crate) fn environment() -> Self {
        Self {
            faults: std::env::var("CPF_FILE_IO_FAULTS").unwrap_or_default(),
            read_size: std::env::var("CPF_FILE_IO_READ_SIZE")
                .ok()
                .and_then(|v| v.parse().ok()),
            read_chunk: std::env::var("CPF_FILE_IO_READ_CHUNK")
                .ok()
                .and_then(|v| v.parse().ok()),
            listing_position: std::env::var("CPF_FILE_IO_LISTING_POSITION")
                .ok()
                .and_then(|v| v.parse().ok()),
            log: std::env::var_os("CPF_FILE_IO_LOG").map(PathBuf::from),
            ..Self::default()
        }
    }

    pub(crate) fn at(&self, point: &'static str, size: usize) -> io::Result<()> {
        let count = {
            let mut calls = self.calls.lock().unwrap_or_else(|e| e.into_inner());
            let count = calls.entry(point).or_default();
            *count += 1;
            *count
        };
        if let Some(log) = &self.log {
            let mut file = std::fs::OpenOptions::new()
                .append(true)
                .create(true)
                .open(log)?;
            writeln!(
                file,
                "{point} {count} {size} {:?}",
                std::thread::current().id()
            )?;
            if let Some(budget) = &self.budget {
                let usage = budget.usage();
                writeln!(
                    file,
                    "file_io.charged {count} {} {:?}",
                    usage.retained + usage.reserved,
                    std::thread::current().id()
                )?;
                writeln!(
                    file,
                    "file_io.limit {count} {} {:?}",
                    budget.limit(),
                    std::thread::current().id()
                )?;
            }
        }
        if let Some((observer, execution)) = &self.observation {
            (observer.0)(crate::jobs::engine::Point::FileIo {
                execution: *execution,
                point,
                size,
            });
        }
        crate::storage::crash::barrier(point);
        for fault in self.faults.split(',') {
            let Some((selected, error)) = fault.split_once('=') else {
                continue;
            };
            if selected != point {
                continue;
            }
            let error = match error.split_once('@') {
                Some((error, occurrence)) => {
                    if occurrence.parse::<u64>().ok() != Some(count) {
                        continue;
                    }
                    error
                }
                None => error,
            };
            if let Some(errno) = errno(error) {
                return Err(io::Error::from_raw_os_error(errno));
            }
        }
        Ok(())
    }

    pub(crate) fn read(&self, source: &mut impl Read, buffer: &mut [u8]) -> io::Result<usize> {
        self.at("file_io.read", buffer.len())?;
        let len = self
            .read_chunk
            .unwrap_or(buffer.len())
            .max(1)
            .min(buffer.len());
        if let Some(size) = self.read_size {
            let mut read = self.read.lock().unwrap_or_else(|e| e.into_inner());
            let len = size.saturating_sub(*read).min(len as u64) as usize;
            buffer[..len].fill(b'x');
            *read += len as u64;
            Ok(len)
        } else {
            source.read(&mut buffer[..len])
        }
    }
}

fn errno(name: &str) -> Option<i32> {
    #[cfg(unix)]
    match name {
        "EIO" => return Some(libc::EIO),
        "ENOSPC" => return Some(libc::ENOSPC),
        "EACCES" => return Some(libc::EACCES),
        "EPERM" => return Some(libc::EPERM),
        "EINTR" => return Some(libc::EINTR),
        "ENOENT" => return Some(libc::ENOENT),
        _ => {}
    }
    // Windows codes whose io::ErrorKind matches the errno's (the stub
    // filesystem of process-free tests): ERROR_IO_DEVICE, ERROR_DISK_FULL,
    // ERROR_ACCESS_DENIED, WSAEINTR (Interrupted), ERROR_FILE_NOT_FOUND.
    #[cfg(windows)]
    match name {
        "EIO" => return Some(1117),
        "ENOSPC" => return Some(112),
        "EACCES" | "EPERM" => return Some(5),
        "EINTR" => return Some(10004),
        "ENOENT" => return Some(2),
        _ => {}
    }
    name.parse().ok()
}

// Field order in Directory drops File before this guard. A zero count observes
// the descriptor's close, not merely entry into Directory's destructor.
static DIRECTORIES: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
#[derive(Debug)]
pub(crate) struct DirectoryCount;
impl DirectoryCount {
    pub(crate) fn new() -> Self {
        DIRECTORIES.fetch_add(1, std::sync::atomic::Ordering::AcqRel);
        Self
    }
}
impl Drop for DirectoryCount {
    fn drop(&mut self) {
        DIRECTORIES.fetch_sub(1, std::sync::atomic::Ordering::AcqRel);
    }
}
pub(crate) fn directories() -> usize {
    DIRECTORIES.load(std::sync::atomic::Ordering::Acquire)
}
