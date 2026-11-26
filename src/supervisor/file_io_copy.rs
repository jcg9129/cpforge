//! Bounded extraction shared by the native directory and process-free fixtures.
use crate::report::{CollectionFailure, IoScratchDetail, OutputFile};
use sha2::{Digest, Sha256};
use std::fs::{File, OpenOptions};
#[cfg(not(feature = "storage-test-hooks"))]
use std::io::Read;
use std::io::{self, Write};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

fn io_failure(e: io::Error) -> OutputFile {
    OutputFile::NotCollected {
        reason: CollectionFailure::Io,
        os_error: e.raw_os_error(),
    }
}
fn close(file: File) -> io::Result<()> {
    #[cfg(unix)]
    {
        crate::supervisor::sys::file_io::close(file)
    }
    #[cfg(not(unix))]
    {
        drop(file);
        Ok(())
    }
}

pub(crate) fn extract(
    source: File,
    budget: u64,
    destination: &Path,
    retained: &AtomicU64,
    warnings: &mut Vec<IoScratchDetail>,
    #[cfg(feature = "storage-test-hooks")] hooks: &crate::supervisor::file_io_hooks::Hooks,
) -> OutputFile {
    let mut copy = BoundedCopy {
        written: 0,
        created: false,
    };
    let outcome = copy
        .run(
            source,
            budget,
            destination,
            retained,
            #[cfg(feature = "storage-test-hooks")]
            hooks,
        )
        .unwrap_or_else(io_failure);
    if copy.created && !matches!(outcome, OutputFile::Collected { .. }) {
        #[cfg(feature = "storage-test-hooks")]
        let removed = hooks
            .at("file_io.partial.unlink", 0)
            .and_then(|()| std::fs::remove_file(destination));
        #[cfg(not(feature = "storage-test-hooks"))]
        let removed = std::fs::remove_file(destination);
        match removed {
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
        #[cfg(feature = "storage-test-hooks")] hooks: &crate::supervisor::file_io_hooks::Hooks,
    ) -> io::Result<OutputFile> {
        #[cfg(feature = "storage-test-hooks")]
        hooks.at("file_io.destination", 0)?;
        let mut output = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(destination)?;
        self.created = true;
        let mut hash = Sha256::new();
        let mut buffer = [0; 64 * 1024];
        loop {
            let len = (budget - self.written).min(buffer.len() as u64) as usize;
            #[cfg(feature = "storage-test-hooks")]
            if len == 0 {
                hooks.at("file_io.probe", self.written as usize)?;
            }
            // At the cap, probe into memory only; the retained file never exceeds its reservation.
            let n = loop {
                #[cfg(feature = "storage-test-hooks")]
                let result = hooks.read(&mut source, &mut buffer[..len.max(1)]);
                #[cfg(not(feature = "storage-test-hooks"))]
                let result = source.read(&mut buffer[..len.max(1)]);
                match result {
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
                #[cfg(feature = "storage-test-hooks")]
                let write = hooks
                    .at("file_io.write", pending.len())
                    .and_then(|()| output.write(pending));
                #[cfg(not(feature = "storage-test-hooks"))]
                let write = output.write(pending);
                let n = match write {
                    Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                    result => result?,
                };
                if n == 0 {
                    return Err(io::ErrorKind::WriteZero.into());
                }
                hash.update(&pending[..n]);
                self.written += n as u64;
                retained.fetch_add(n as u64, Ordering::AcqRel);
                #[cfg(feature = "storage-test-hooks")]
                {
                    hooks.at("file_io.copied", self.written as usize)?;
                    hooks.at("file_io.copy_length", output.metadata()?.len() as usize)?;
                }
                pending = &pending[n..];
            }
        }
        output.flush()?;
        close(output)?;
        #[cfg(feature = "storage-test-hooks")]
        hooks.at("file_io.extracted", 0)?;
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
