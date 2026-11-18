//! Blocking sink operations, called exclusively by reserved sink workers.

use std::fs::File;
use std::io::{self, Write};
use std::path::Path;

#[derive(Debug)]
pub(in crate::supervisor) struct CaptureFile(Option<File>);

impl CaptureFile {
    pub(in crate::supervisor) fn create(path: &Path) -> io::Result<Self> {
        File::create(path).map(|file| Self(Some(file)))
    }

    pub(in crate::supervisor) fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0.as_mut().expect("open sink").write(bytes)
    }

    pub(in crate::supervisor) fn finish(&mut self) -> io::Result<()> {
        loop {
            match self.0.as_mut().expect("open sink").flush() {
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                result => {
                    result?;
                    break;
                }
            }
        }
        let file = self.0.take().expect("close once");
        #[cfg(unix)]
        {
            use std::os::fd::IntoRawFd;
            // SAFETY: into_raw_fd transfers the unique descriptor. Do not retry
            // close on EINTR: Linux has already released the descriptor.
            if unsafe { libc::close(file.into_raw_fd()) } == -1 {
                return Err(io::Error::last_os_error());
            }
        }
        #[cfg(windows)]
        {
            use std::os::windows::io::IntoRawHandle;
            // SAFETY: into_raw_handle transfers unique ownership to this close.
            if unsafe { windows_sys::Win32::Foundation::CloseHandle(file.into_raw_handle()) } == 0 {
                return Err(io::Error::last_os_error());
            }
        }
        Ok(())
    }
}
