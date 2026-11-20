//! Private pre-lock captures. Unlike workspace publication temporaries, these
//! belong in the OS temporary directory and are never part of lock recovery.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Seek};
use std::path::{Path, PathBuf};

/// Exact bytes captured before taking the workspace lock. Named sources may be
/// streams (including FIFOs); only published test data must be regular files.
/// Drop closes the handle before best-effort removal, also on capture failure.
#[derive(Debug)]
pub struct CapturedFile {
    path: PathBuf,
    file: Option<File>,
}

impl CapturedFile {
    pub fn from_reader(reader: &mut impl Read) -> io::Result<Self> {
        Self::from_reader_in(reader, &std::env::temp_dir())
    }

    fn from_reader_in(reader: &mut impl Read, directory: &Path) -> io::Result<Self> {
        let (path, file) = loop {
            let mut bits = [0_u8; 16];
            getrandom::fill(&mut bits).map_err(|error| io::Error::other(error.to_string()))?;
            let path = directory.join(format!(".cpf-capture-{:032x}", u128::from_le_bytes(bits)));
            let mut options = OpenOptions::new();
            options.read(true).write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            #[cfg(windows)]
            {
                use std::os::windows::fs::OpenOptionsExt;
                // Retain private access until all duplicated capture handles close.
                options.share_mode(0);
            }
            match options.open(&path) {
                Ok(file) => break (path, file),
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(error),
            }
        };
        let mut capture = Self {
            path,
            file: Some(file),
        };
        crate::storage::copy_stream(reader, capture.file.as_mut().expect("capture handle"))?;
        Ok(capture)
    }

    /// `-` consumes stdin. An explicitly named path is opened as a stream,
    /// following links and allowing non-regular sources without a size cap.
    pub fn from_source(path: &Path) -> io::Result<Self> {
        if path == Path::new("-") {
            Self::from_reader(&mut io::stdin().lock())
        } else {
            Self::from_reader(&mut File::open(path)?)
        }
    }

    pub(super) fn reader(&self) -> io::Result<File> {
        let mut file = self.file.as_ref().expect("capture handle").try_clone()?;
        file.rewind()?;
        Ok(file)
    }
}

impl Drop for CapturedFile {
    fn drop(&mut self) {
        drop(self.file.take());
        let _ = fs::remove_file(&self.path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_read_error_after_some_bytes_removes_the_partial_capture() {
        struct FailingSource(bool);
        impl Read for FailingSource {
            fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
                if self.0 {
                    return Err(io::Error::other("source failed after bytes"));
                }
                self.0 = true;
                buffer[..3].copy_from_slice(b"abc");
                Ok(3)
            }
        }
        let mut bits = [0_u8; 16];
        getrandom::fill(&mut bits).unwrap();
        let directory = std::env::temp_dir().join(format!(
            "cpforge-capture-test-{:032x}",
            u128::from_le_bytes(bits),
        ));
        fs::create_dir(&directory).unwrap();
        let error =
            CapturedFile::from_reader_in(&mut FailingSource(false), &directory).unwrap_err();
        assert!(error.to_string().contains("source failed after bytes"));
        assert_eq!(fs::read_dir(&directory).unwrap().count(), 0);
        fs::remove_dir(directory).unwrap();
    }

    #[test]
    fn captures_are_private_rewindable_and_removed_after_handles_close() {
        let bytes = b"\0binary\r\n\xff\n";
        let capture = CapturedFile::from_reader(&mut &bytes[..]).unwrap();
        let path = capture.path.clone();
        assert_eq!(path.parent(), Some(std::env::temp_dir().as_path()));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                capture
                    .file
                    .as_ref()
                    .unwrap()
                    .metadata()
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o077,
                0
            );
        }
        for _ in 0..2 {
            let mut actual = Vec::new();
            capture.reader().unwrap().read_to_end(&mut actual).unwrap();
            assert_eq!(actual, bytes);
        }
        drop(capture);
        assert!(!path.exists());
    }
}
