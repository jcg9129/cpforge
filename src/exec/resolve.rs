//! Program selection under the requested platform, independent of host paths.
//!
//! These stages are not `ProgramSpec` or `ResolvedExecutionSpec`: admission
//! still supplies CPF_* variables, stdin, sinks and normalized limits. Shell
//! basename validation and positional-argument lowering live in `shell`; batch
//! launch escaping and exact-path creation belong to B4a.
//!
//! Containment checks both raw and normalized components before rejecting dot
//! components in generated paths, so neither `art/../tool` nor `./art/tool`
//! bypasses deferral validation. Diagnostics retain missing candidates as well
//! as non-launchable files. An approximate-check flag covers every attempted
//! check, including rejected candidates, rather than only the selected file.

use std::ffi::{OsStr, OsString};
use std::fmt;
use std::io;
use std::path::{Path, PathBuf};

use crate::config::{
    Deadline, Limits, Origin, Platform, Provenance, Sourced, StageCommand, StageKind, StagePlan,
    StreamCap, TextValue, Token, env_name_fold_os,
    syntax::{join, native_absolute},
};

use super::{Executable, PreparedCommand, PreparedStage, Probe, ProbeResult, ProgramRequest};

/// Creation strategy selected independently of producer scheduling.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LaunchKind {
    Native,
    Batch,
}

/// Why a PATH or PATHEXT segment was excluded.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EntryReason {
    Empty,
    Relative,
    MalformedExtension,
}

/// An excluded environment segment, in its original order.
#[derive(Debug)]
pub struct SkippedEntry {
    /// Original segment, including any quotes in Windows PATH.
    pub entry: OsString,
    pub reason: EntryReason,
}

/// Why a probed candidate could not be selected.
#[derive(Debug)]
pub enum CandidateReason {
    NotFound,
    Directory,
    Other,
    NotExecutable,
    UnsupportedExtension,
    /// The original error, including any native error code.
    Io(io::Error),
}

impl fmt::Display for CandidateReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotFound => f.write_str("not found"),
            Self::Directory => f.write_str("directory"),
            Self::Other => f.write_str("not a regular file"),
            Self::NotExecutable => f.write_str("not executable for the effective user"),
            Self::UnsupportedExtension => f.write_str("extension is not launchable on Windows"),
            Self::Io(error) => write!(f, "probe error: {error}"),
        }
    }
}

/// An attempted absolute path and its rejection.
#[derive(Debug)]
pub struct SkippedCandidate {
    pub path: PathBuf,
    pub reason: CandidateReason,
}

/// Ordered diagnostics, also retained when no candidate is selected. Missing
/// candidates are recorded too. Approximation covers all attempted checks,
/// including a rejected candidate before the chosen one.
#[derive(Debug, Default)]
pub struct ResolutionTrace {
    pub skipped_path_entries: Vec<SkippedEntry>,
    pub skipped_pathext_entries: Vec<SkippedEntry>,
    pub skipped_candidates: Vec<SkippedCandidate>,
    pub approximate_check: bool,
}

/// Selection and diagnostics for one original program request.
#[derive(Debug)]
pub struct ResolutionRecord {
    pub request: ProgramRequest,
    pub chosen_path: PathBuf,
    pub launch_kind: LaunchKind,
    pub trace: ResolutionTrace,
}

/// Absolute selected path and the launch strategy, with its audit record.
#[derive(Debug)]
pub struct ResolvedProgram {
    pub path: PathBuf,
    pub launch_kind: LaunchKind,
    pub record: ResolutionRecord,
}

/// Resolved program with unchanged arguments and literal shell parts.
#[derive(Debug)]
#[allow(clippy::large_enum_variant)] // Keep literal shell metadata directly accessible.
pub enum ResolvedCommand {
    Argv {
        program: ResolvedProgram,
        args: Vec<OsString>,
    },
    Shell {
        interpreter: ResolvedProgram,
        interpreter_args: Vec<OsString>,
        script: Sourced<String>,
        args: Vec<OsString>,
    },
}

/// Programs selected, shell parts preserved, admission still outstanding.
#[derive(Debug)]
pub struct ResolvedStage {
    pub kind: StageKind,
    pub command: ResolvedCommand,
    pub cwd: PathBuf,
    pub shared_cwd: bool,
    pub environment: Vec<(OsString, OsString)>,
    pub deadline: Deadline,
    pub stdout: StreamCap,
    pub stderr: StreamCap,
    pub limits: Limits,
    pub plan: StagePlan,
}

/// Immediate resolution or a consumer waiting for its producer.
#[derive(Debug)]
pub enum Resolution {
    Resolved(ResolvedStage),
    AwaitingProducer(DeferredStage),
}

/// Private pending state exposes no launchable program. The orchestrator calls
/// `complete` only after the known producer succeeded; this type cannot observe
/// producer success itself. Completion uses the frozen stage environment.
#[derive(Debug)]
pub struct DeferredStage {
    prepared: PreparedStage,
    platform: Platform,
}

impl DeferredStage {
    /// Apply explicit-path resolution after successful production. Consumes
    /// the pending state and never reads the process environment.
    pub fn complete(self, probe: &dyn Probe) -> Result<ResolvedStage, ResolveErrors> {
        resolve_now(self.prepared, probe, self.platform)
    }
}

/// Distinct policy failures, separate from candidate observations.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResolveErrorKind {
    NotFound,
    NotAbsolute,
    TrailingDotOrSpace,
    GeneratedDotComponent,
    OwnProducer,
    GeneratedInterpreter,
}

impl fmt::Display for ResolveErrorKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::NotFound => "no launchable program found",
            Self::NotAbsolute => "explicit program path must be absolute",
            Self::TrailingDotOrSpace => "Windows program name ends in a dot or space",
            Self::GeneratedDotComponent => "generated program path contains . or ..",
            Self::OwnProducer => "build stage cannot produce its own program",
            Self::GeneratedInterpreter => "shell interpreter cannot be generated",
        })
    }
}

/// A failure tied to the C1 program leaf and its complete search diagnostics.
#[derive(Debug)]
pub struct ResolveError {
    pub stage: StageKind,
    pub field: String,
    pub request: ProgramRequest,
    pub origin: Origin,
    pub provenance: Provenance,
    pub kind: ResolveErrorKind,
    pub trace: ResolutionTrace,
}

impl fmt::Display for ResolveError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}: {:?} stage, {} ({:?}): {}",
            self.origin, self.stage, self.field, self.request, self.kind
        )?;
        for entry in &self.trace.skipped_path_entries {
            write!(f, "; PATH {:?}: {:?}", entry.entry, entry.reason)?;
        }
        for entry in &self.trace.skipped_pathext_entries {
            write!(f, "; PATHEXT {:?}: {:?}", entry.entry, entry.reason)?;
        }
        for candidate in &self.trace.skipped_candidates {
            write!(f, "; {}: {}", candidate.path.display(), candidate.reason)?;
        }
        Ok(())
    }
}

/// All independent resolution errors for one stage.
#[derive(Debug)]
pub struct ResolveErrors {
    pub errors: Vec<ResolveError>,
}

impl fmt::Display for ResolveErrors {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (index, error) in self.errors.iter().enumerate() {
            if index != 0 {
                writeln!(f)?;
            }
            write!(f, "{error}")?;
        }
        Ok(())
    }
}
impl std::error::Error for ResolveErrors {}

/// Resolve the single program of an argv or shell stage. All independent
/// policy errors for its program are collected before any filesystem access.
/// PATH/PATHEXT come exclusively from the prepared environment.
pub fn resolve(
    prepared: PreparedStage,
    probe: &dyn Probe,
    platform: Platform,
) -> Result<Resolution, ResolveErrors> {
    let (request, text, shell) = request(&prepared);
    let mut errors: Vec<ResolveError> = Vec::new();
    if platform == Platform::Windows
        && final_component(request_value(&request), platform)
            .last()
            .is_some_and(|b| matches!(b, b'.' | b' '))
    {
        errors.push(error(
            &prepared,
            text,
            &request,
            ResolveErrorKind::TrailingDotOrSpace,
        ));
    }
    if let ProgramRequest::Path(path) = &request {
        if !native_absolute(path.as_os_str(), platform) {
            errors.push(error(
                &prepared,
                text,
                &request,
                ResolveErrorKind::NotAbsolute,
            ));
        }
    }
    let generated = match (&request, &prepared.artifact_dir) {
        (ProgramRequest::Path(path), Some(root)) => generated_path(path, root, platform),
        _ => false,
    };
    if generated && shell {
        // An interpreter is external only; dot or producer rules do not apply.
        errors.push(error(
            &prepared,
            text,
            &request,
            ResolveErrorKind::GeneratedInterpreter,
        ));
    } else if generated {
        if components(request_value(&request), platform)
            .iter()
            .any(|part| matches!(*part, b"." | b".."))
        {
            errors.push(error(
                &prepared,
                text,
                &request,
                ResolveErrorKind::GeneratedDotComponent,
            ));
        }
        if prepared.kind == StageKind::Build {
            errors.push(error(
                &prepared,
                text,
                &request,
                ResolveErrorKind::OwnProducer,
            ));
        }
    }
    if !errors.is_empty() {
        return Err(ResolveErrors { errors });
    }
    if generated {
        Ok(Resolution::AwaitingProducer(DeferredStage {
            prepared,
            platform,
        }))
    } else {
        resolve_now(prepared, probe, platform).map(Resolution::Resolved)
    }
}

fn resolve_now(
    prepared: PreparedStage,
    probe: &dyn Probe,
    platform: Platform,
) -> Result<ResolvedStage, ResolveErrors> {
    let (request, text, _) = request(&prepared);
    let program = select(&request, &prepared.environment, probe, platform).map_err(|trace| {
        let mut failure = error(&prepared, text, &request, ResolveErrorKind::NotFound);
        failure.trace = trace;
        ResolveErrors {
            errors: vec![failure],
        }
    })?;
    let command = match prepared.command {
        PreparedCommand::Argv { args, .. } => ResolvedCommand::Argv { program, args },
        PreparedCommand::Shell {
            interpreter,
            script,
            args,
        } => ResolvedCommand::Shell {
            interpreter: program,
            interpreter_args: interpreter
                .args
                .into_iter()
                .map(|arg| OsString::from(arg.value))
                .collect(),
            script,
            args,
        },
    };
    Ok(ResolvedStage {
        kind: prepared.kind,
        command,
        cwd: prepared.cwd,
        shared_cwd: prepared.shared_cwd,
        environment: prepared.environment,
        deadline: prepared.deadline,
        stdout: prepared.stdout,
        stderr: prepared.stderr,
        limits: prepared.limits,
        plan: prepared.plan,
    })
}

fn request(prepared: &PreparedStage) -> (ProgramRequest, &TextValue, bool) {
    match (&prepared.command, &prepared.plan.command) {
        (PreparedCommand::Argv { program, .. }, StageCommand::Argv(argv)) => {
            (program.clone(), &argv[0], false)
        }
        (PreparedCommand::Shell { interpreter, .. }, _) => {
            let text = &interpreter.program;
            let program = if let Some(path) = &text.path {
                ProgramRequest::Path(path.clone())
            } else {
                // C1 guarantees a literal lookup here, without brace expansion.
                let mut literal = OsString::new();
                for token in &text.tokens {
                    if let Token::Literal(value) = token {
                        literal.push(value);
                    }
                }
                if text.lookup {
                    ProgramRequest::Lookup(literal)
                } else {
                    ProgramRequest::Path(literal.into())
                }
            };
            (program, text, true)
        }
        _ => unreachable!("prepared command retains its originating plan"),
    }
}

fn error(
    prepared: &PreparedStage,
    text: &TextValue,
    request: &ProgramRequest,
    kind: ResolveErrorKind,
) -> ResolveError {
    ResolveError {
        stage: prepared.kind,
        field: text.origin.key_path.clone(),
        request: request.clone(),
        origin: text.origin.clone(),
        provenance: text.provenance.clone(),
        kind,
        trace: ResolutionTrace::default(),
    }
}

fn request_value(request: &ProgramRequest) -> &OsStr {
    match request {
        ProgramRequest::Path(path) => path.as_os_str(),
        ProgramRequest::Lookup(name) => name,
    }
}

fn environment<'a>(
    entries: &'a [(OsString, OsString)],
    name: &str,
    platform: Platform,
) -> Option<&'a OsStr> {
    let name = env_name_fold_os(platform, OsStr::new(name));
    entries
        .iter()
        .rev()
        .find(|(key, _)| env_name_fold_os(platform, key) == name)
        .map(|(_, value)| value.as_os_str())
}

fn select(
    request: &ProgramRequest,
    environment_entries: &[(OsString, OsString)],
    probe: &dyn Probe,
    platform: Platform,
) -> Result<ResolvedProgram, ResolutionTrace> {
    let mut trace = ResolutionTrace::default();
    let extensions: Vec<OsString> = if platform == Platform::Windows {
        let value = environment(environment_entries, "PATHEXT", platform)
            .filter(|value| !value.is_empty())
            .unwrap_or(OsStr::new(".COM;.EXE;.BAT;.CMD"));
        split(value, b';')
            .into_iter()
            .filter_map(|entry| {
                let bytes = entry.as_encoded_bytes();
                if bytes.len() < 2
                    || bytes[0] != b'.'
                    || bytes.iter().any(|b| separator(*b, platform))
                {
                    trace.skipped_pathext_entries.push(SkippedEntry {
                        entry,
                        reason: EntryReason::MalformedExtension,
                    });
                    None
                } else {
                    Some(entry)
                }
            })
            .collect()
    } else {
        Vec::new()
    };
    let bases: Vec<PathBuf> = match request {
        ProgramRequest::Path(path) => vec![path.clone()],
        ProgramRequest::Lookup(name) => {
            let value = environment(environment_entries, "PATH", platform);
            let entries: Vec<(OsString, OsString)> = match value {
                None => Vec::new(),
                Some(value) if platform == Platform::Windows => split_windows_path(value),
                Some(value) => split(value, b':')
                    .into_iter()
                    .map(|entry| (entry.clone(), entry))
                    .collect(),
            };
            entries
                .into_iter()
                .filter_map(|(entry, directory)| {
                    let reason = if directory.is_empty() {
                        Some(EntryReason::Empty)
                    } else if !native_absolute(&directory, platform) {
                        Some(EntryReason::Relative)
                    } else {
                        None
                    };
                    if let Some(reason) = reason {
                        trace
                            .skipped_path_entries
                            .push(SkippedEntry { entry, reason });
                        None
                    } else {
                        Some(join(Path::new(&directory), name, platform))
                    }
                })
                .collect()
        }
    };
    for base in bases {
        let mut candidates: Vec<PathBuf> = Vec::new();
        if platform != Platform::Windows || extension(base.as_os_str(), platform).is_some() {
            candidates.push(base);
        } else {
            if matches!(request, ProgramRequest::Path(_)) {
                candidates.push(base.clone());
            }
            for extension in &extensions {
                let mut candidate = base.as_os_str().to_owned();
                candidate.push(extension);
                candidates.push(candidate.into());
            }
        }
        for path in candidates {
            let reason = match probe.probe(&path) {
                Ok(ProbeResult::NotFound) => Some(CandidateReason::NotFound),
                Ok(ProbeResult::Directory) => Some(CandidateReason::Directory),
                Ok(ProbeResult::Other) => Some(CandidateReason::Other),
                Err(error) => Some(CandidateReason::Io(error)),
                Ok(ProbeResult::Regular(executable)) => {
                    if platform == Platform::Windows {
                        windows_launch_kind(path.as_os_str())
                            .is_none()
                            .then_some(CandidateReason::UnsupportedExtension)
                    } else {
                        let allowed = match executable {
                            Executable::Yes => true,
                            Executable::No => false,
                            Executable::Approximate(allowed) => {
                                trace.approximate_check = true;
                                allowed
                            }
                        };
                        (!allowed).then_some(CandidateReason::NotExecutable)
                    }
                }
            };
            if let Some(reason) = reason {
                trace
                    .skipped_candidates
                    .push(SkippedCandidate { path, reason });
            } else {
                let launch_kind = if platform == Platform::Windows {
                    windows_launch_kind(path.as_os_str()).expect("launchable extension")
                } else {
                    LaunchKind::Native
                };
                return Ok(ResolvedProgram {
                    path: path.clone(),
                    launch_kind,
                    record: ResolutionRecord {
                        request: request.clone(),
                        chosen_path: path,
                        launch_kind,
                        trace,
                    },
                });
            }
        }
    }
    Err(trace)
}

fn windows_launch_kind(path: &OsStr) -> Option<LaunchKind> {
    match extension(path, Platform::Windows) {
        Some(ext) if ext.eq_ignore_ascii_case(b".exe") || ext.eq_ignore_ascii_case(b".com") => {
            Some(LaunchKind::Native)
        }
        Some(ext) if ext.eq_ignore_ascii_case(b".bat") || ext.eq_ignore_ascii_case(b".cmd") => {
            Some(LaunchKind::Batch)
        }
        _ => None,
    }
}

fn separator(byte: u8, platform: Platform) -> bool {
    byte == b'/' || (platform == Platform::Windows && byte == b'\\')
}

pub(super) fn final_component(path: &OsStr, platform: Platform) -> &[u8] {
    path.as_encoded_bytes()
        .rsplit(|b| separator(*b, platform))
        .next()
        .unwrap_or_default()
}

fn extension(path: &OsStr, platform: Platform) -> Option<&[u8]> {
    let name = final_component(path, platform);
    name.iter()
        .rposition(|b| *b == b'.')
        .map(|pos| &name[pos..])
}

/// Split only at ASCII delimiters; native non-Unicode segments survive intact.
fn split(value: &OsStr, delimiter: u8) -> Vec<OsString> {
    value
        .as_encoded_bytes()
        .split(|b| *b == delimiter)
        .map(|bytes| encoded(bytes).to_owned())
        .collect()
}

/// Windows PATH convention (also `std::env::split_paths`): `;` outside double
/// quotes separates segments, and every quote is removed from the directory.
/// Each item is the original segment and the directory it names.
fn split_windows_path(value: &OsStr) -> Vec<(OsString, OsString)> {
    let bytes = value.as_encoded_bytes();
    let mut segments: Vec<&[u8]> = Vec::new();
    let mut start = 0;
    let mut quoted = false;
    for (index, byte) in bytes.iter().enumerate() {
        match byte {
            b'"' => quoted = !quoted,
            b';' if !quoted => {
                segments.push(&bytes[start..index]);
                start = index + 1;
            }
            _ => {}
        }
    }
    segments.push(&bytes[start..]);
    segments
        .into_iter()
        .map(|segment| {
            let mut directory = OsString::new();
            for piece in segment.split(|b| *b == b'"') {
                // Pushing rejoins pieces with the native encoding's rules.
                directory.push(encoded(piece));
            }
            (encoded(segment).to_owned(), directory)
        })
        .collect()
}

fn encoded(bytes: &[u8]) -> &OsStr {
    // SAFETY: callers pass slices of one OsStr's encoded bytes from this
    // process, cut only next to ASCII bytes, which are self-synchronizing.
    unsafe { OsStr::from_encoded_bytes_unchecked(bytes) }
}

fn components(path: &OsStr, platform: Platform) -> Vec<&[u8]> {
    path.as_encoded_bytes()
        .split(|b| separator(*b, platform))
        .filter(|p| !p.is_empty())
        .collect()
}

fn normalized<'a>(parts: &[&'a [u8]], anchor: usize) -> Vec<&'a [u8]> {
    let mut result: Vec<&'a [u8]> = Vec::new();
    for &part in parts {
        match part {
            b"." => {}
            b".." => {
                if result.len() > anchor {
                    result.pop();
                }
            }
            _ => result.push(part),
        }
    }
    result
}

fn generated_path(path: &Path, root: &Path, platform: Platform) -> bool {
    // No existing platform-parameterized workspace-containment helper exists.
    // This uses storage §7's ASCII component comparison on Windows (Unix is
    // byte-exact here). Raw containment also rejects art/../escape as generated;
    // normalized containment catches /out/./art/tool without accepting dots.
    let path_bytes = path.as_os_str().as_encoded_bytes();
    let root_bytes = root.as_os_str().as_encoded_bytes();
    let unc = |bytes: &[u8]| -> bool {
        platform == Platform::Windows
            && bytes.len() >= 2
            && separator(bytes[0], platform)
            && separator(bytes[1], platform)
    };
    if unc(path_bytes) != unc(root_bytes) {
        return false;
    }
    let path = components(path.as_os_str(), platform);
    let root = components(root.as_os_str(), platform);
    let contains = |path: &[&[u8]], root: &[&[u8]]| -> bool {
        path.len() >= root.len()
            && path.iter().zip(root).all(|(left, right)| {
                if platform == Platform::Windows {
                    left.eq_ignore_ascii_case(right)
                } else {
                    left == right
                }
            })
    };
    let anchor = if platform != Platform::Windows {
        0
    } else if unc(path_bytes) {
        2
    } else {
        1
    };
    contains(&path, &root) || contains(&normalized(&path, anchor), &normalized(&root, anchor))
}

#[cfg(test)]
mod tests;
