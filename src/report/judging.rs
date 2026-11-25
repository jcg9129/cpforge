//! Finalized batch judging evidence and fixed-point scores (report §§2.11–2.13).
//! Producers populate these values only after their records are final.

use super::{ExecutionResult, RunClass};
use serde::{Deserialize, Serialize};

macro_rules! tokens {
    ($name:ident { $($variant:ident),+ $(,)? }) => {
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
        #[serde(rename_all = "snake_case")]
        pub enum $name { $($variant),+ }
    };
}
tokens!(JudgeRole {
    Checker,
    Validator,
    Interactor
});
tokens!(JudgeProfile { Testlib, Simple });
tokens!(JudgeFail {
    WrongAnswer,
    PresentationError
});
tokens!(JudgeFailClass {
    DeclaredFail,
    ProtocolUnsupported,
    ReportInvalid,
    ArtefactInvalid,
    Crashed,
    TestDataError
});
tokens!(JudgeReason {
    DeclaredFail,
    ExitUnsupported,
    OutcomeUnsupported,
    PointsNotEnabled,
    OutsideAuthority,
    ReportMissing,
    ReportNotRegular,
    ReportUnreadable,
    ReportTooLarge,
    ReportUndecodable,
    ReportMalformed,
    ReportSchema,
    PointsInvalid,
    Disagreement,
    ToutMissing,
    ToutNotRegular,
    ToutUnreadable,
    ToutTooLarge,
    Signal,
    ExitCode,
    WallDeadline,
    OutputBudget,
    DialogueOutput,
    PeerAllowanceExpired,
    InvalidInput,
    ValidationFailed
});
tokens!(JudgeNotRead {
    Stopped,
    CleanupIncomplete,
    ExtractionFailed
});
tokens!(CheckingUnchecked {
    CheckingUnverified,
    CheckingInterrupted
});
tokens!(ValidationOutcome {
    Valid,
    Invalid,
    ValidationFailed,
    ValidatorFailed,
    NotRun
});
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum JudgeEncoding {
    #[serde(rename = "windows_1251")]
    Windows1251,
    #[serde(rename = "utf_8")]
    Utf8,
}
tokens!(Aggregation { Sum, Min });

/// Nonnegative nanounits, bounded by the schema's fourteen integer digits.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Score(i128);
impl Score {
    pub const UNIT: i128 = 1_000_000_000;
    pub const MAX: i128 = 100_000_000_000_000 * Self::UNIT - 1;
    pub fn new(nanounits: i128) -> Option<Self> {
        (0..=Self::MAX)
            .contains(&nanounits)
            .then_some(Self(nanounits))
    }
    pub const fn nanounits(self) -> i128 {
        self.0
    }
    pub fn parse(value: &str) -> Option<Self> {
        let (whole, fraction) = value.split_once('.').unwrap_or((value, ""));
        if whole.is_empty()
            || whole.len() > 14
            || !whole.bytes().all(|v| v.is_ascii_digit())
            || (whole.len() > 1 && whole.starts_with('0'))
            || fraction.len() > 9
            || !fraction.bytes().all(|v| v.is_ascii_digit())
            || (value.contains('.') && (fraction.is_empty() || fraction.ends_with('0')))
        {
            return None;
        }
        let fraction = if fraction.is_empty() {
            0
        } else {
            fraction.parse::<i128>().ok()? * 10_i128.pow(9 - fraction.len() as u32)
        };
        Self::new(whole.parse::<i128>().ok()? * Self::UNIT + fraction)
    }
}
impl std::fmt::Display for Score {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0 / Self::UNIT)?;
        let remainder = self.0 % Self::UNIT;
        if remainder != 0 {
            let digits = format!("{remainder:09}");
            write!(f, ".{}", digits.trim_end_matches('0'))?;
        }
        Ok(())
    }
}
impl Serialize for Score {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.to_string())
    }
}
impl<'de> Deserialize<'de> for Score {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let value = String::deserialize(d)?;
        Self::parse(&value).ok_or_else(|| serde::de::Error::custom("noncanonical score"))
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Scoring {
    PassFail,
    Points {
        scale: Score,
        aggregation: Aggregation,
    },
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AggregateScore {
    Points {
        aggregation: Aggregation,
        value: Score,
        max: Score,
        full: bool,
    },
    Incomplete {
        aggregation: Aggregation,
        missing: u32,
    },
    NoTests,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum JudgeResult {
    Passed,
    Rejected {
        fail: JudgeFail,
    },
    Scored {
        raw: Score,
        scale: Score,
    },
    JudgeFailed {
        class: JudgeFailClass,
        reason: JudgeReason,
    },
    Unverified,
    NotRead {
        reason: JudgeNotRead,
    },
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum JudgedCheck {
    Rejected {
        fail: JudgeFail,
        role: JudgeRole,
    },
    Scored {
        raw: Score,
        scale: Score,
        role: JudgeRole,
    },
    JudgeFailed {
        class: JudgeFailClass,
        role: JudgeRole,
        reason: JudgeReason,
    },
    Unchecked {
        reason: CheckingUnchecked,
    },
}
/// Summary evidence keeps a capped head and the whole stream's length and hash.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct JudgeText {
    pub head: Vec<u8>,
    pub len: u64,
    pub sha256: [u8; 32],
}
impl JudgeText {
    /// The `nsum(P)` rule of report §3.3: the head is cut back to a character
    /// boundary only when the whole value is `utf8`-eligible.
    pub fn new(bytes: &[u8], cap: usize) -> Self {
        let eligible = std::str::from_utf8(bytes)
            .ok()
            .filter(|v| !v.chars().any(|c| c <= '\u{1f}' || c == '\u{7f}'));
        Self::cut(bytes, cap, eligible)
    }
    /// A decoded `testlib` message keeps its first 4 096 bytes as UTF-8.
    pub fn message(text: &str) -> Self {
        Self::cut(text.as_bytes(), 4096, Some(text))
    }
    fn cut(bytes: &[u8], cap: usize, text: Option<&str>) -> Self {
        use sha2::{Digest, Sha256};
        let mut end = bytes.len().min(cap);
        if let Some(text) = text {
            while !text.is_char_boundary(end) {
                end -= 1;
            }
        }
        Self {
            head: bytes[..end].to_vec(),
            len: bytes.len() as u64,
            sha256: Sha256::digest(bytes).into(),
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Validation {
    pub execution: u32,
    pub profile: JudgeProfile,
    pub outcome: ValidationOutcome,
    pub stderr: Option<JudgeText>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct JudgeReport {
    pub path: Option<String>,
    pub bytes: u64,
    pub encoding: Option<JudgeEncoding>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Judge {
    pub role: JudgeRole,
    pub execution: u32,
    pub profile: JudgeProfile,
    pub result: Option<JudgeResult>,
    pub message: Option<JudgeText>,
    pub stderr: Option<JudgeText>,
    pub report: Option<JudgeReport>,
}
/// Parsed report evidence is supplied in validation-stage order. This module
/// does not read files, decode XML, or infer extraction failures from text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum JudgeRead {
    Io {
        os_error: Option<i32>,
    },
    Invalid(JudgeReason),
    Parsed {
        outcome: TestlibOutcome,
        raw: Option<Score>,
    },
}
tokens!(TestlibOutcome {
    Accepted,
    WrongAnswer,
    PresentationError,
    Fail,
    Points,
    Unsupported
});
#[derive(Clone, Debug)]
pub struct JudgeInput {
    pub execution: ExecutionResult,
    pub profile: JudgeProfile,
    pub cleanup_unverified: bool,
    pub read: JudgeRead,
}
impl JudgeReason {
    pub fn from_class(class: &RunClass) -> Option<Self> {
        Some(match class {
            RunClass::WallDeadline => Self::WallDeadline,
            RunClass::OutputBudget { .. } => Self::OutputBudget,
            RunClass::Signal { .. } => Self::Signal,
            RunClass::NonzeroExit { .. } => Self::ExitCode,
            _ => return None,
        })
    }
}

tokens!(CheckMode {
    Compare,
    Checker,
    Interactive
});
tokens!(InteractiveChecking {
    Interactor,
    Checker,
    InteractorChecker
});
tokens!(InteractiveMode { Relay, Direct });
tokens!(SeedMode { None, Env, Argv });
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct InteractiveConfig {
    pub checking: InteractiveChecking,
    pub mode: InteractiveMode,
    #[serde(with = "optional_duration")]
    pub idle_us: Option<u64>,
    pub early_reader_close: bool,
    #[serde(with = "duration")]
    pub interactor_allowance_us: u64,
    #[serde(with = "duration")]
    pub peer_allowance_us: u64,
    pub solution_seed: SeedMode,
    pub interactor_seed: SeedMode,
}
mod duration {
    use serde::{Deserialize, Deserializer, Serializer};
    pub fn serialize<S: Serializer>(v: &u64, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&v.to_string())
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<u64, D::Error> {
        let v = String::deserialize(d)?;
        super::super::wire::read::read_u64(&serde_json::Value::String(v))
            .map_err(serde::de::Error::custom)
    }
}
mod optional_duration {
    use serde::{Deserialize, Deserializer, Serializer};
    pub fn serialize<S: Serializer>(v: &Option<u64>, s: S) -> Result<S::Ok, S::Error> {
        serde::Serialize::serialize(&v.map(|v| v.to_string()), s)
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Option<u64>, D::Error> {
        Option::<String>::deserialize(d)?
            .map(|v| -> Result<u64, D::Error> {
                super::super::wire::read::read_u64(&serde_json::Value::String(v))
                    .map_err(serde::de::Error::custom)
            })
            .transpose()
    }
}
tokens!(AnswerPolicy { File, None });
tokens!(DisplayProfile { Cpf, JudgeCf });
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheckConfig {
    pub kind: CheckMode,
    pub answer: AnswerPolicy,
    pub scoring: Scoring,
    pub display: DisplayProfile,
    pub interactive: Option<InteractiveConfig>,
}
impl Default for CheckConfig {
    fn default() -> Self {
        Self {
            kind: CheckMode::Compare,
            answer: AnswerPolicy::File,
            scoring: Scoring::PassFail,
            display: DisplayProfile::Cpf,
            interactive: None,
        }
    }
}
