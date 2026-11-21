use super::{Answer, Health, Identifier, Record, TestStore, open_regular, read_catalog};
use crate::selection::record_result;
use crate::storage::inputs::{FrozenFile, InputsError, JobInputs};
use std::fmt;
use std::path::{Path, PathBuf};

pub type TestId = Identifier;

/// Extension point for resolved per-test settings, before any overrides exist.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct TestSettings {}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FrozenAnswer {
    Absent,
    /// A zero length is a deliberately present, empty answer.
    Present {
        path: PathBuf,
        len: u64,
        sha256: [u8; 32],
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FrozenTest {
    pub id: TestId,
    pub input: FrozenFile,
    pub answer: FrozenAnswer,
    pub settings: TestSettings,
}

#[derive(Debug)]
pub struct FrozenManifest {
    entries: Vec<FrozenTest>,
}
impl FrozenManifest {
    pub fn entries(&self) -> &[FrozenTest] {
        &self.entries
    }
}

#[derive(Debug)]
pub enum FreezeError {
    Catalog(String),
    UnknownId(TestId),
    DisabledId(TestId),
    /// The same catalog entry was selected more than once.
    DuplicateId(TestId),
    Unhealthy {
        path: PathBuf,
        health: Health,
    },
    Inputs(InputsError),
}
impl fmt::Display for FreezeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Catalog(message) => write!(f, "cannot freeze tests: {message}"),
            Self::UnknownId(id) => write!(f, "test {id} is unknown or unregistered"),
            Self::DisabledId(id) => write!(f, "selected test {id} is disabled"),
            Self::DuplicateId(id) => write!(f, "test {id} is selected more than once"),
            Self::Unhealthy { path, health } => write!(f, "{}: {health}", path.display()),
            Self::Inputs(error) => write!(f, "{error}"),
        }
    }
}
impl std::error::Error for FreezeError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Inputs(error) => Some(error),
            _ => None,
        }
    }
}
impl From<InputsError> for FreezeError {
    fn from(error: InputsError) -> Self {
        Self::Inputs(error)
    }
}

impl TestStore {
    /// Resolve registered, enabled ids exactly in the caller's order. Candidates
    /// never qualify. A failed freeze returns no manifest and cannot be retried
    /// in the same job, even through a newly borrowed inputs capability.
    /// Explicitly selected disabled ids are errors: the caller filters disabled
    /// entries out of default selections. Selecting one entry twice is a
    /// `DuplicateId` error, detected before any file is opened or copied.
    pub fn freeze_into(
        &self,
        ids: &[TestId],
        inputs: &JobInputs<'_>,
    ) -> Result<FrozenManifest, FreezeError> {
        inputs.finish(self.freeze_selected(ids, inputs))
    }

    fn freeze_selected(
        &self,
        ids: &[TestId],
        inputs: &JobInputs<'_>,
    ) -> Result<FrozenManifest, FreezeError> {
        inputs.ensure_active()?;
        let path = self.catalog_path();
        let catalog = match read_catalog(&path) {
            Record::Missing => super::Catalog::default(),
            record => record_result(record).map_err(|reason| {
                FreezeError::Catalog(format!("{} is {reason}", path.display()))
            })?,
        };
        let mut chosen = vec![false; catalog.entries.len()];
        let mut selected = Vec::with_capacity(ids.len());
        for id in ids {
            let index = catalog
                .entries
                .iter()
                .position(|entry| entry.id == *id)
                .ok_or_else(|| FreezeError::UnknownId(id.clone()))?;
            let entry = &catalog.entries[index];
            if !entry.enabled {
                return Err(FreezeError::DisabledId(id.clone()));
            }
            if std::mem::replace(&mut chosen[index], true) {
                return Err(FreezeError::DuplicateId(id.clone()));
            }
            selected.push(entry);
        }
        let mut entries = Vec::with_capacity(selected.len());
        for entry in selected {
            let copy = |source: &str, extension: &str| -> Result<FrozenFile, FreezeError> {
                let path = self.directory.join(source);
                let mut file = open_regular(&path)
                    .map_err(|health| FreezeError::Unhealthy { path, health })?;
                inputs
                    .copy(Path::new(&format!("{}.{extension}", entry.id)), &mut file)
                    .map_err(FreezeError::Inputs)
            };
            let input = copy(&entry.input, "in")?;
            let answer = match &entry.answer {
                Answer::Absent => FrozenAnswer::Absent,
                Answer::Named(path) => {
                    let file = copy(path, "ans")?;
                    FrozenAnswer::Present {
                        path: file.path,
                        len: file.len,
                        sha256: file.sha256,
                    }
                }
            };
            entries.push(FrozenTest {
                id: entry.id.clone(),
                input,
                answer,
                settings: TestSettings::default(),
            });
        }
        Ok(FrozenManifest { entries })
    }
}
