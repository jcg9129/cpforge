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
    let config = wire::encode_judge_fingerprint_config(check)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;
    bytes(out, b"cpf-fp-judge-v1")?;
    bytes(out, &config)?;
    for (token, role) in ["checker", "validator", "interactor"]
        .into_iter()
        .zip(roles)
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
