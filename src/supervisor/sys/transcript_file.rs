//! Transcript paths are created before admission; workers own only regular files.
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufWriter, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;

fn create_new(path: &Path) -> io::Result<BufWriter<File>> {
    let file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK)
        .open(path)?;
    if !file.metadata()?.is_file() {
        return Err(io::Error::other("transcript must be a new regular file"));
    }
    Ok(BufWriter::new(file))
}

pub(in crate::supervisor) struct TranscriptFiles {
    pub directions: [TranscriptFile; 2],
    pub events: TranscriptEvents,
}
impl TranscriptFiles {
    pub(in crate::supervisor) fn create(directory: &Path) -> io::Result<Self> {
        fs::create_dir_all(directory)?;
        Ok(Self {
            directions: [
                TranscriptFile::create(directory, "s2i")?,
                TranscriptFile::create(directory, "i2s")?,
            ],
            events: TranscriptEvents(create_new(&directory.join("events.jsonl"))?),
        })
    }
}

pub(in crate::supervisor) struct TranscriptFile {
    raw: BufWriter<File>,
    index: BufWriter<File>,
}
impl TranscriptFile {
    fn create(directory: &Path, name: &str) -> io::Result<Self> {
        Ok(Self {
            raw: create_new(&directory.join(format!("{name}.bin")))?,
            index: create_new(&directory.join(format!("{name}.jsonl")))?,
        })
    }
    pub(in crate::supervisor) fn write(&mut self, bytes: &[u8], json: &[u8]) -> io::Result<()> {
        self.raw.write_all(bytes)?;
        self.index.write_all(json)?;
        self.index.write_all(b"\n")
    }
    pub(in crate::supervisor) fn finish(&mut self) -> io::Result<()> {
        self.raw.flush()?;
        self.index.flush()
    }
}
pub(in crate::supervisor) struct TranscriptEvents(BufWriter<File>);
impl TranscriptEvents {
    pub(in crate::supervisor) fn write(&mut self, json: &[u8]) -> io::Result<()> {
        self.0.write_all(json)?;
        self.0.flush()
    }
}
