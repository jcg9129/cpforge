//! Finalized role evidence. Header provenance describes a captured candidate;
//! it makes no claim about the header selected by a compiler.
use super::{InfrastructureCauses, Invariant, JudgeProfile, JudgeText, RunClass, SkipReason};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RoleName {
    Checker,
    Validator,
    Interactor,
    Generator,
    Brute,
}
impl RoleName {
    pub const fn wire_token(self) -> &'static str {
        match self {
            Self::Checker => "checker",
            Self::Validator => "validator",
            Self::Interactor => "interactor",
            Self::Generator => "generator",
            Self::Brute => "brute",
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HeaderReason {
    None,
    Ambiguous,
    NotBesideSources,
    AngleInclude,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Header {
    Candidate {
        path: JudgeText,
        sha256: [u8; 32],
        pinned: bool,
    },
    NotIdentified {
        reason: HeaderReason,
    },
    NotApplicable,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RoleProvenance {
    Built { header: Header },
    Declared,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RoleOrigin {
    Built {
        language: String,
        entry: Option<JudgeText>,
    },
    Prebuilt {
        program: JudgeText,
    },
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RoleCapture {
    pub checked: bool,
    pub sources: Vec<JudgeText>,
    pub sources_dropped: u32,
    pub support_files: Vec<JudgeText>,
    pub support_files_dropped: u32,
    pub counts: [u32; 2],
    pub bytes: u64,
    pub lists_sha256: [u8; 32],
    pub content_sha256: [u8; 32],
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RoleBuildResult {
    Succeeded,
    Cancelled,
    Infrastructure { causes: InfrastructureCauses },
    Failed { cause: Box<RoleBuildCause> },
    InternalError { invariant: Invariant },
    Skipped { reason: SkipReason },
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RoleBuildCause {
    Run(RunClass),
    MissingOutput {
        names: Vec<JudgeText>,
        names_dropped: u32,
    },
    GeneratedProgramInvalid {
        name: JudgeText,
        reason: String,
    },
    OutputTooLarge {
        bytes: u64,
        cap: u64,
    },
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RoleBuild {
    pub execution: u32,
    pub build_id: Option<String>,
    pub output_root: Option<String>,
    pub result: RoleBuildResult,
    pub artifacts: Vec<JudgeText>,
    pub artifacts_dropped: u32,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RoleReport {
    pub role: RoleName,
    pub profile: Option<JudgeProfile>,
    pub origin: RoleOrigin,
    pub build: Option<RoleBuild>,
    pub capture: RoleCapture,
    pub provenance: RoleProvenance,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JudgeScratchDetail {
    StagedCopyKept,
    TranscriptKept,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TestlibHeaderDetail {
    NotIdentified,
    NotPinned,
    Declared,
}

impl RoleReport {
    /// Producers can use this evidence-only rule without inspecting a compiler.
    pub fn header_warning(&self) -> Option<TestlibHeaderDetail> {
        if self.profile != Some(JudgeProfile::Testlib) {
            return None;
        }
        match &self.provenance {
            RoleProvenance::Declared => Some(TestlibHeaderDetail::Declared),
            RoleProvenance::Built {
                header: Header::Candidate { pinned: false, .. },
            } => Some(TestlibHeaderDetail::NotPinned),
            RoleProvenance::Built {
                header: Header::NotIdentified { .. },
            } => Some(TestlibHeaderDetail::NotIdentified),
            _ => None,
        }
    }
}
#[derive(Clone, Debug)]
pub struct CaptureRole {
    pub role: RoleName,
    pub files: Vec<std::path::PathBuf>,
}
