//! Workspace §3.2 byte encodings. File contents stream through a fixed buffer;
//! paths/settings alone are retained in memory. No execution expansion occurs.
use std::ffi::OsStr;
use std::fs::File;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use crate::config::{Cwd, EnvEdit, StageCommand, StagePlan, TargetPlan};
use crate::report::wire::native_bytes;
use crate::storage::inputs::CapturedSourceCopy;
use crate::tests_store::{FrozenAnswer, FrozenTest};

mod judge;
pub use judge::*;

/// A logical problem-relative name and the file supplying its bytes. Status
/// can use original paths; a job must use its captured copy paths.
#[derive(Clone, Debug)]
pub struct SourceFile {
    pub problem_path: PathBuf,
    pub content_path: PathBuf,
}
impl From<&CapturedSourceCopy> for SourceFile {
    fn from(file: &CapturedSourceCopy) -> Self {
        Self {
            problem_path: file.problem_path.components().iter().collect(),
            content_path: file.copy_path.clone(),
        }
    }
}

/// Hash captured copies (or the corresponding original files for status).
/// The configured lists come from the unexpanded, validated target plan.
pub fn source_fingerprint(files: &[SourceFile], target: &TargetPlan) -> io::Result<[u8; 32]> {
    let mut hash = HashWriter(Sha256::new());
    encode_source(&mut hash, files, target)?;
    Ok(hash.0.finalize().into())
}

/// Content evidence for a role, using the workspace file-list encoding only.
/// Includes sources, support, assets and a prebuilt program, sorted by path.
pub(crate) fn captured_content_fingerprint(files: &[CapturedSourceCopy]) -> io::Result<[u8; 32]> {
    let mut hash = HashWriter(Sha256::new());
    let mut ordered: Vec<_> = files
        .iter()
        .map(|file| {
            let path: PathBuf = file.problem_path.components().iter().collect();
            (path_bytes(&path), file)
        })
        .collect();
    ordered.sort_by(|a, b| a.0.cmp(&b.0));
    count(&mut hash, ordered.len() as u64)?;
    for (path, file) in ordered {
        bytes(&mut hash, &path)?;
        contents(&mut hash, &file.copy_path)?;
    }
    Ok(hash.0.finalize().into())
}

/// Hash the selected frozen tests, ordered by id bytes, including the
/// distinction between an absent answer and a present empty answer.
pub fn tests_fingerprint(tests: &[FrozenTest]) -> io::Result<[u8; 32]> {
    let mut hash = HashWriter(Sha256::new());
    encode_tests(&mut hash, tests)?;
    Ok(hash.0.finalize().into())
}

struct HashWriter(Sha256);
impl Write for HashWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0.update(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
fn count(out: &mut impl Write, value: u64) -> io::Result<()> {
    out.write_all(&value.to_le_bytes())
}
fn bytes(out: &mut impl Write, value: &[u8]) -> io::Result<()> {
    count(out, value.len() as u64)?;
    out.write_all(value)
}
fn native(out: &mut impl Write, value: &OsStr) -> io::Result<()> {
    bytes(out, &native_bytes(value))
}
fn path_bytes(path: &Path) -> Vec<u8> {
    let mut result = Vec::new();
    for component in path.components() {
        if !result.is_empty() {
            result.push(b'/');
        }
        result.extend(native_bytes(component.as_os_str()));
    }
    result
}
fn contents(out: &mut impl Write, path: &Path) -> io::Result<()> {
    let mut file = File::open(path)?;
    let metadata = file.metadata()?;
    if !metadata.is_file() {
        return Err(io::Error::other("fingerprint input is not a regular file"));
    }
    let len = metadata.len();
    count(out, len)?;
    let mut remaining = len;
    let mut buffer = [0; 64 * 1024];
    while remaining > 0 {
        let limit = remaining.min(buffer.len() as u64) as usize;
        let read = match file.read(&mut buffer[..limit]) {
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            result => result?,
        };
        if read == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "fingerprint input shrank",
            ));
        }
        out.write_all(&buffer[..read])?;
        remaining -= read as u64;
    }
    let extra = loop {
        match file.read(&mut [0; 1]) {
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            result => break result?,
        }
    };
    if extra != 0 {
        return Err(io::Error::other("fingerprint input grew"));
    }
    Ok(())
}
fn encode_source(
    out: &mut impl Write,
    files: &[SourceFile],
    target: &TargetPlan,
) -> io::Result<()> {
    bytes(out, b"cpf-fp-source-v1")?;
    let mut files: Vec<_> = files
        .iter()
        .map(|f| (path_bytes(&f.problem_path), &f.content_path))
        .collect();
    files.sort_by(|a, b| a.0.cmp(&b.0));
    count(out, files.len() as u64)?;
    for (path, file) in files {
        bytes(out, &path)?;
        contents(out, file)?;
    }
    // The profile name precedes the covered build settings; no profile is the
    // empty string (profile names are non-empty identifiers).
    bytes(
        out,
        target
            .profile
            .as_ref()
            .map_or("", |p| p.value.as_str())
            .as_bytes(),
    )?;
    for list in [&target.sources, &target.support_files] {
        count(out, list.len() as u64)?;
        for file in list {
            bytes(out, file.value.components().join("/").as_bytes())?;
        }
    }
    match &target.entry {
        None => out.write_all(&[0])?,
        Some(entry) => {
            out.write_all(&[1])?;
            bytes(out, entry.value.as_bytes())?;
        }
    }
    match &target.build {
        None => out.write_all(&[0]),
        Some(stage) => {
            out.write_all(&[1])?;
            encode_build(out, stage)
        }
    }
}
fn encode_build(out: &mut impl Write, stage: &StagePlan) -> io::Result<()> {
    match &stage.command {
        StageCommand::Argv(argv) => {
            out.write_all(&[0])?;
            count(out, argv.len() as u64)?;
            for arg in argv {
                native(out, &arg.unexpanded)?;
            }
        }
        StageCommand::Shell {
            interpreter,
            script,
            args,
        } => {
            out.write_all(&[1])?;
            count(out, 1 + interpreter.args.len() as u64)?;
            native(out, &interpreter.program.unexpanded)?;
            for arg in &interpreter.args {
                bytes(out, arg.value.as_bytes())?;
            }
            bytes(out, script.value.as_bytes())?;
            count(out, args.len() as u64)?;
            for arg in args {
                native(out, &arg.unexpanded)?;
            }
        }
    }
    match &stage.cwd.value {
        Cwd::Run => out.write_all(&[0, 0])?,
        Cwd::Problem => out.write_all(&[0, 1])?,
        Cwd::Workspace => out.write_all(&[0, 2])?,
        Cwd::Path(path) => {
            out.write_all(&[1])?;
            native(out, &path.unexpanded)?;
        }
    }
    let mut remove = Vec::new();
    let mut env = Vec::new();
    for edit in &stage.environment {
        match edit {
            EnvEdit::Remove(name) => remove.push(name.value.as_bytes()),
            EnvEdit::Unset(name) => env.push((name.value.as_bytes(), None)),
            EnvEdit::Set { name, value } => {
                env.push((name.value.as_bytes(), Some(value.value.as_bytes())))
            }
        }
    }
    remove.sort_unstable();
    env.sort_unstable_by_key(|entry| entry.0);
    count(out, remove.len() as u64)?;
    for name in remove {
        bytes(out, name)?;
    }
    count(out, env.len() as u64)?;
    for (name, value) in env {
        bytes(out, name)?;
        match value {
            None => out.write_all(&[0])?,
            Some(value) => {
                out.write_all(&[1])?;
                bytes(out, value)?;
            }
        }
    }
    Ok(())
}
fn encode_tests(out: &mut impl Write, tests: &[FrozenTest]) -> io::Result<()> {
    bytes(out, b"cpf-fp-tests-v1")?;
    let mut tests: Vec<_> = tests.iter().collect();
    tests.sort_by(|a, b| a.id.as_str().as_bytes().cmp(b.id.as_str().as_bytes()));
    count(out, tests.len() as u64)?;
    for test in tests {
        bytes(out, test.id.as_str().as_bytes())?;
        contents(out, &test.input.path)?;
        match &test.answer {
            FrozenAnswer::Absent => out.write_all(&[0])?,
            FrozenAnswer::Present { path, .. } => {
                out.write_all(&[1])?;
                contents(out, path)?;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;
