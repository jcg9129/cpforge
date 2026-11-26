//! Judge preparation and frozen capture diagnostics. No simple-profile result
//! is read from cwd; all evidence is outside it before scratch measurement.
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use super::ExecutionRecord;
use crate::jobs::roles::StagedFile;
use crate::report::{JudgeText, RunNote};
use crate::storage::budget::{JobBudget, Reservation};
use crate::storage::job::JobOwner;
use crate::supervisor::CancelToken;

#[derive(Debug)]
struct StagedCopy {
    path: std::path::PathBuf,
    // Keep the original containing directory even if the judge renames cwd.
    #[cfg(unix)]
    parent: File,
    reservation: Reservation,
    bytes: u64,
    removed: bool,
}
impl StagedCopy {
    fn remove(&self) -> io::Result<()> {
        #[cfg(unix)]
        {
            use std::os::fd::AsRawFd;
            use std::os::unix::ffi::OsStrExt;
            let name =
                std::ffi::CString::new(self.path.file_name().expect("staged name").as_bytes())?;
            // SAFETY: the directory descriptor and NUL-terminated basename are
            // live. unlinkat never follows the final entry, including a link.
            if unsafe { libc::unlinkat(self.parent.as_raw_fd(), name.as_ptr(), 0) } == 0 {
                return Ok(());
            }
            Err(io::Error::last_os_error())
        }
        #[cfg(not(unix))]
        fs::remove_file(&self.path)
    }
}
impl Drop for StagedCopy {
    fn drop(&mut self) {
        // A refused/stalled cleanup still retains its tool-created bytes.
        let _ = self.reservation.commit(self.bytes);
    }
}

#[derive(Debug)]
pub(super) struct Scratch {
    copies: Vec<StagedCopy>,
    _owner: JobOwner,
}
impl Scratch {
    pub(super) fn new(owner: JobOwner) -> Self {
        Self {
            copies: Vec::new(),
            _owner: owner,
        }
    }

    pub(super) fn stage(
        &mut self,
        files: &[StagedFile],
        budget: &Arc<JobBudget>,
        cancel: Option<&CancelToken>,
    ) -> io::Result<()> {
        for staged in files {
            if cancel.is_some_and(CancelToken::is_cancelled) {
                return Err(io::Error::new(
                    io::ErrorKind::Interrupted,
                    "staging cancelled",
                ));
            }
            let mut source = staged.source.as_deref().map(open_regular).transpose()?;
            let bytes = source
                .as_ref()
                .map(|f| f.metadata().map(|m| m.len()))
                .transpose()?
                .unwrap_or(0);
            let reservation = budget
                .reserve(bytes)
                .map_err(|e| io::Error::other(e.to_string()))?;
            let parent = staged.destination.parent().expect("staged parent");
            fs::create_dir_all(parent)?;
            #[cfg(unix)]
            let parent = {
                use std::os::unix::fs::OpenOptionsExt;
                OpenOptions::new()
                    .read(true)
                    .custom_flags(libc::O_NOFOLLOW | libc::O_DIRECTORY | libc::O_CLOEXEC)
                    .open(parent)?
            };
            let mut output = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&staged.destination)?;
            // Register the partial copy before writing; every error keeps exact
            // accounting and allows post-run scratch cleanup.
            self.copies.push(StagedCopy {
                path: staged.destination.clone(),
                #[cfg(unix)]
                parent,
                reservation,
                bytes: 0,
                removed: false,
            });
            let copy = self.copies.last_mut().expect("registered copy");
            if let Some(source) = &mut source {
                let mut remaining = bytes;
                let mut buffer = [0; 64 * 1024];
                while remaining != 0 {
                    if cancel.is_some_and(CancelToken::is_cancelled) {
                        return Err(io::Error::new(
                            io::ErrorKind::Interrupted,
                            "staging cancelled",
                        ));
                    }
                    let len = remaining.min(buffer.len() as u64) as usize;
                    source.read_exact(&mut buffer[..len])?;
                    let mut pending = &buffer[..len];
                    while !pending.is_empty() {
                        let n = output.write(pending)?;
                        if n == 0 {
                            return Err(io::ErrorKind::WriteZero.into());
                        }
                        copy.bytes += n as u64;
                        pending = &pending[n..];
                    }
                    remaining -= len as u64;
                }
            }
        }
        Ok(())
    }

    fn bytes(&self) -> u64 {
        self.copies.iter().map(|c| c.bytes).sum()
    }

    /// Remove every staged copy still present; returns the bytes kept. A copy
    /// already gone (removed earlier or by the judge) is released.
    pub(super) fn clear(&mut self) -> u64 {
        let mut kept = 0;
        for copy in self.copies.iter_mut().filter(|c| !c.removed) {
            match copy.remove() {
                Err(e) if e.kind() != io::ErrorKind::NotFound => kept += copy.bytes,
                _ => {
                    copy.bytes = 0;
                    copy.removed = true;
                }
            }
        }
        kept
    }
}

fn open_regular(path: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC);
    }
    let file = options.open(path)?;
    if !file.metadata()?.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "staged file is not regular",
        ));
    }
    Ok(file)
}

fn diagnostic(record: Option<&ExecutionRecord>) -> io::Result<Option<JudgeText>> {
    use sha2::{Digest, Sha256};
    let Some(ExecutionRecord::Started(root)) = record else {
        return Ok(None);
    };
    let Some(capture) = root.stderr.as_ref().filter(|c| c.stored != 0) else {
        return Ok(None);
    };
    let Some(path) = &capture.sink else {
        return Ok(Some(JudgeText::new(&capture.retained, 1024)));
    };
    let mut file = open_regular(path)?.take(capture.stored);
    let mut hash = Sha256::new();
    let mut head = Vec::new();
    let mut len = 0;
    let mut utf8 = Vec::new();
    let mut eligible = true;
    let mut buffer = [0; 64 * 1024];
    loop {
        let n = file.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        hash.update(&buffer[..n]);
        if eligible {
            utf8.extend_from_slice(&buffer[..n]);
            let end = match std::str::from_utf8(&utf8) {
                Ok(_) => utf8.len(),
                Err(error) if error.error_len().is_none() => error.valid_up_to(),
                Err(_) => {
                    eligible = false;
                    0
                }
            };
            if eligible {
                eligible = !utf8[..end].iter().any(|b| *b <= 0x1f || *b == 0x7f);
                drop(utf8.drain(..end));
            }
        }
        head.extend_from_slice(&buffer[..n.min(1024 - head.len())]);
        len += n as u64;
    }
    if eligible && utf8.is_empty() {
        while std::str::from_utf8(&head).is_err() {
            let _ = head.pop();
        }
    }
    let mut text = JudgeText::new(&head, 1024);
    text.len = len;
    text.sha256 = hash.finalize().into();
    Ok(Some(text))
}

/// Judge evidence and scratch outcome of one finished run.
#[derive(Debug)]
pub(super) struct Finished {
    pub(super) stderr: Option<JudgeText>,
    /// Staged bytes still in `cwd/`, excluded from its measurement.
    pub(super) kept_bytes: u64,
    /// Some staged copy could not be confirmed removed (`judge_scratch`).
    pub(super) kept: bool,
    /// `Err` while a scratch worker may still own `cwd/` paths: the cwd step
    /// must not start a competing measurement or removal.
    pub(super) cwd: Result<(), RunNote>,
}

/// Execution §2.1 steps (1)–(2) for a judge run, after its cleanup result: a
/// simple judge has no artefact in `cwd/` to extract, so its frozen stderr
/// diagnostic is read and its staged copies are removed, on a credited
/// post-run worker. The caller then measures `cwd/` (step 3).
pub(super) async fn finish(
    owner: JobOwner,
    record: Option<Arc<ExecutionRecord>>,
    scratch: Option<Scratch>,
    walking: &Arc<AtomicBool>,
) -> Finished {
    let bytes = scratch.as_ref().map_or(0, Scratch::bytes);
    let staged = scratch.as_ref().is_some_and(|s| !s.copies.is_empty());
    let unfinished = |note| Finished {
        stderr: None,
        kept_bytes: bytes,
        kept: staged,
        cwd: Err(note),
    };
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    {
        use std::sync::atomic::Ordering;
        // An earlier post-run worker of this job is blocked past its deadline:
        // never stack another credited thread on the same stalled filesystem.
        // The scratch is then dropped here without I/O and stays accounted.
        if walking
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return unfinished(RunNote::CwdUnmeasuredTimeout);
        }
        let walking = super::cwd::Walking(walking.clone());
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
        // Removal is attempted even after cancellation: the wait uses its own
        // token. The credited worker owns the lease and reservations if the
        // caller stops waiting.
        crate::supervisor::post_run(&CancelToken::new(), deadline, move || {
            let _walking = walking;
            let _owner = owner;
            let stderr = diagnostic(record.as_deref()).ok().flatten();
            let mut scratch = scratch;
            let kept_bytes = scratch.as_mut().map_or(0, Scratch::clear);
            let kept = scratch.is_some_and(|s| s.copies.iter().any(|c| !c.removed));
            Finished {
                stderr,
                kept_bytes,
                kept,
                cwd: Ok(()),
            }
        })
        .await
        .unwrap_or_else(unfinished)
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        // Production refuses judged execution on these platforms; inline
        // executors can still exercise the evidence adapters.
        let _ = (owner, walking, unfinished);
        let stderr = diagnostic(record.as_deref()).ok().flatten();
        let mut scratch = scratch;
        let kept_bytes = scratch.as_mut().map_or(0, Scratch::clear);
        let kept = scratch.is_some_and(|s| s.copies.iter().any(|c| !c.removed));
        Finished {
            stderr,
            kept_bytes,
            kept,
            cwd: Ok(()),
        }
    }
}
