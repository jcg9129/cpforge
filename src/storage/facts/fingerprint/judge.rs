//! Pure encoding over copied bytes and composed, unexpanded role settings.
//! Freezing and status are responsible for supplying the corresponding inputs.
use super::{HashWriter, bytes, count, encode_build, path_bytes};
use crate::config::StagePlan;
use crate::report::{CheckConfig, JudgeProfile, SeedMode, wire};
use sha2::{Digest, Sha256};
use std::io::{self, Write};
use std::path::PathBuf;

#[derive(Clone, Debug)]
pub struct JudgeFingerprintFile {
    pub path: PathBuf,
    pub copied_bytes: Vec<u8>,
}

#[derive(Clone, Debug)]
pub struct JudgeFingerprintBuilt {
    pub sources: Vec<PathBuf>,
    pub support_files: Vec<PathBuf>,
    pub assets: Vec<PathBuf>,
    pub language: String,
    pub entry: Option<String>,
    pub build: Option<StagePlan>,
    pub run: StagePlan,
}

#[derive(Clone, Debug)]
pub enum JudgeFingerprintOrigin {
    Built(Box<JudgeFingerprintBuilt>),
    Prebuilt {
        program: PathBuf,
        assets: Vec<PathBuf>,
    },
}

#[derive(Clone, Debug)]
pub struct JudgeFingerprintRole {
    pub profile: JudgeProfile,
    pub files: Vec<JudgeFingerprintFile>,
    pub origin: JudgeFingerprintOrigin,
    /// Declared overrides, distinct from defaults in the composed stages.
    pub deadline_us: Option<u64>,
    pub stdout_cap: Option<u64>,
    pub stderr_cap: Option<u64>,
    pub seed: SeedMode,
}

impl JudgeFingerprintRole {
    /// The same composed, unexpanded settings for freeze and read-only status.
    pub(crate) fn from_spec(spec: &crate::config::RoleSpec) -> Self {
        use crate::config::{RoleInput, Sourced, TargetPlan};
        let paths = |paths: &[Sourced<String>]| -> Vec<PathBuf> {
            paths.iter().map(|p| p.value.split('/').collect()).collect()
        };
        let origin = match &spec.input {
            RoleInput::Built(b) => {
                let target = Box::new(TargetPlan::for_role(spec));
                JudgeFingerprintOrigin::Built(Box::new(JudgeFingerprintBuilt {
                    sources: paths(&b.sources),
                    support_files: paths(&b.support_files),
                    assets: paths(&spec.assets),
                    language: b.language.value.clone(),
                    entry: b.entry.as_ref().map(|e| e.value.clone()),
                    build: target.build.clone(),
                    run: target.run.clone(),
                }))
            }
            RoleInput::Prebuilt(p) => JudgeFingerprintOrigin::Prebuilt {
                program: p.program.value.split('/').collect(),
                assets: paths(&spec.assets),
            },
        };
        Self {
            profile: spec.profile.value,
            files: Vec::new(),
            origin,
            deadline_us: spec.deadline_ms.as_ref().map(|d| d.value * 1000),
            stdout_cap: spec.stdout_cap.as_ref().map(|c| c.value),
            stderr_cap: spec.stderr_cap.as_ref().map(|c| c.value),
            seed: spec.seed.as_ref().map_or(SeedMode::None, |s| s.value),
        }
    }
}

/// Read current role files with no admission, tool resolution or filesystem writes.
/// The streaming encoder is also used for frozen job-owned copies.
pub(crate) fn current_judge_fingerprint(
    check: Option<&crate::config::CheckConfig>,
    dir: &std::path::Path,
) -> io::Result<Option<[u8; 32]>> {
    use crate::config::ProblemPath;
    use crate::storage::inputs::{CapturedSourceCopy, SourceRole};
    let Some(check) = check else { return Ok(None) };
    let mut slots = [None, None, None];
    let mut files: [Vec<CapturedSourceCopy>; 3] = std::array::from_fn(|_| Vec::new());
    for (index, spec) in [
        check.checker.as_deref(),
        check.validator.as_deref(),
        check.interactive.as_ref().map(|i| &i.interactor),
    ]
    .into_iter()
    .enumerate()
    {
        let Some(spec) = spec else { continue };
        slots[index] = Some(JudgeFingerprintRole::from_spec(spec));
        for path in spec.captured_paths() {
            let problem_path = ProblemPath::from_role(path);
            let relative: PathBuf = problem_path.components().iter().collect();
            let content_path = crate::storage::checked_path(dir, &relative, false)
                .map_err(|e| io::Error::other(e.to_string()))?;
            let file = crate::tests_store::open_regular(&content_path)
                .map_err(|e| io::Error::other(format!("{}: {e}", relative.display())))?;
            files[index].push(CapturedSourceCopy {
                role: SourceRole::Asset,
                problem_path,
                copy_path: content_path,
                len: file.metadata()?.len(),
                sha256: [0; 32],
            });
        }
    }
    let copies = files.each_ref().map(|files| files.iter().collect());
    judge_fingerprint_from_copies(&check.report_config(), &slots, &copies)
}

/// Slots are checker, validator, interactor, including absent-role tags.
/// A role-free problem has no judge fingerprint.
pub fn judge_fingerprint(
    check: &CheckConfig,
    roles: &[Option<JudgeFingerprintRole>; 3],
) -> io::Result<Option<[u8; 32]>> {
    if roles.iter().all(Option::is_none) {
        return Ok(None);
    }
    let mut hash = HashWriter(Sha256::new());
    encode_judge(&mut hash, check, roles)?;
    Ok(Some(hash.0.finalize().into()))
}

/// The complete config §9.1.3 input, without process resolution or expansion.
pub fn encode_judge(
    out: &mut impl Write,
    check: &CheckConfig,
    roles: &[Option<JudgeFingerprintRole>; 3],
) -> io::Result<()> {
    encode_judge_files(out, check, roles, None)
}

/// Stream regular role files: frozen copies for jobs, originals for status.
/// Large role files never enter a Vec of bytes.
pub fn judge_fingerprint_from_copies(
    check: &CheckConfig,
    roles: &[Option<JudgeFingerprintRole>; 3],
    copies: &[Vec<&crate::storage::inputs::CapturedSourceCopy>; 3],
) -> io::Result<Option<[u8; 32]>> {
    if roles.iter().all(Option::is_none) {
        return Ok(None);
    }
    let mut hash = HashWriter(Sha256::new());
    encode_judge_files(&mut hash, check, roles, Some(copies))?;
    Ok(Some(hash.0.finalize().into()))
}

fn encode_judge_files(
    out: &mut impl Write,
    check: &CheckConfig,
    roles: &[Option<JudgeFingerprintRole>; 3],
    copies: Option<&[Vec<&crate::storage::inputs::CapturedSourceCopy>; 3]>,
) -> io::Result<()> {
    let config = wire::encode_judge_fingerprint_config(check)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;
    bytes(out, b"cpf-fp-judge-v1")?;
    bytes(out, &config)?;
    for (index, (token, role)) in ["checker", "validator", "interactor"]
        .into_iter()
        .zip(roles)
        .enumerate()
    {
        let Some(role) = role else {
            out.write_all(&[0])?;
            continue;
        };
        out.write_all(&[1])?;
        bytes(out, token.as_bytes())?;
        bytes(
            out,
            match role.profile {
                JudgeProfile::Testlib => b"testlib",
                JudgeProfile::Simple => b"simple",
            },
        )?;
        out.write_all(&[u8::from(matches!(
            role.origin,
            JudgeFingerprintOrigin::Prebuilt { .. }
        ))])?;
        if let Some(copies) = copies {
            let mut files: Vec<_> = copies[index]
                .iter()
                .map(|file| {
                    let path: PathBuf = file.problem_path.components().iter().collect();
                    (path_bytes(&path), *file)
                })
                .collect();
            files.sort_by(|a, b| a.0.cmp(&b.0));
            count(out, files.len() as u64)?;
            for (path, file) in files {
                bytes(out, &path)?;
                count(out, file.len)?;
                let reader = crate::tests_store::open_regular(&file.copy_path)
                    .map_err(|e| io::Error::other(e.to_string()))?;
                let mut reader = std::io::Read::take(reader, file.len);
                if io::copy(&mut reader, out)? != file.len {
                    return Err(io::Error::new(
                        io::ErrorKind::UnexpectedEof,
                        "captured role file changed",
                    ));
                }
            }
        } else {
            let mut files: Vec<_> = role
                .files
                .iter()
                .map(|f| (path_bytes(&f.path), &f.copied_bytes))
                .collect();
            files.sort_by(|a, b| a.0.cmp(&b.0));
            count(out, files.len() as u64)?;
            for (path, contents) in files {
                bytes(out, &path)?;
                bytes(out, contents)?;
            }
        }
        match &role.origin {
            JudgeFingerprintOrigin::Built(built) => {
                for list in [&built.sources, &built.support_files, &built.assets] {
                    paths(out, list)?;
                }
                bytes(out, built.language.as_bytes())?;
                match &built.entry {
                    None => out.write_all(&[0])?,
                    Some(entry) => {
                        out.write_all(&[1])?;
                        bytes(out, entry.as_bytes())?;
                    }
                }
                match &built.build {
                    None => out.write_all(&[0])?,
                    Some(stage) => {
                        out.write_all(&[1])?;
                        encode_build(out, stage)?;
                    }
                }
                encode_build(out, &built.run)?;
            }
            JudgeFingerprintOrigin::Prebuilt { program, assets } => {
                paths(out, std::slice::from_ref(program))?;
                paths(out, assets)?;
            }
        }
        for value in [role.deadline_us, role.stdout_cap, role.stderr_cap] {
            match value {
                None => out.write_all(&[0])?,
                Some(value) => {
                    out.write_all(&[1])?;
                    count(out, value)?;
                }
            }
        }
        bytes(
            out,
            match role.seed {
                SeedMode::None => b"none",
                SeedMode::Env => b"env",
                SeedMode::Argv => b"argv",
            },
        )?;
    }
    Ok(())
}

fn paths(out: &mut impl Write, paths: &[PathBuf]) -> io::Result<()> {
    count(out, paths.len() as u64)?;
    for path in paths {
        bytes(out, &path_bytes(path))?;
    }
    Ok(())
}
