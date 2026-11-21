//! Last completed test runs: schema-first records and ordered publication.
pub mod fingerprint;

use std::path::{Path, PathBuf};

use toml_edit::{DocumentMut, Item, Table, value};

use super::job::{Epoch, JobId};
use super::job_seq::{parse_decimal_u64, read_job_sequence};
use super::{
    Cleanup, Durability, ErrorKind, LockOptions, Record, WorkspaceLock, atomic_replace,
    classify_tool_record, read_tool_record,
};
use crate::jobs::{CaptureCheck, CaptureCheckReason};
use crate::names::Identifier;
use crate::report::JobVerdict;

/// The current configuration model has one unnamed solution target. Profiles
/// select its build settings; they do not create independent fact slots.
pub const SOLUTION_TARGET: &str = "solution";

pub fn fact_path(problem: &Identifier, target: &Identifier) -> PathBuf {
    Path::new(".cpforge/facts")
        .join(problem.as_str())
        .join(format!("{target}.toml"))
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FactRecord {
    pub epoch: Epoch,
    pub n: u64,
    pub job: JobId,
    /// UTC, with millisecond precision, like report timestamps.
    pub finished_at: String,
    pub verdict: JobVerdict,
    pub source: [u8; 32],
    pub tests: [u8; 32],
    pub capture_check: CaptureCheck,
}
impl FactRecord {
    pub fn encode(&self) -> Vec<u8> {
        let mut document = DocumentMut::new();
        document["schema"] = value(1);
        document["epoch"] = value(self.epoch.as_str());
        document["n"] = value(self.n.to_string());
        document["job"] = value(self.job.as_str());
        document["finished_at"] = value(self.finished_at.as_str());
        document["verdict"] = value(self.verdict.wire_token());
        document["source"] = value(hex(&self.source));
        document["tests"] = value(hex(&self.tests));
        let mut capture = Table::new();
        match &self.capture_check {
            CaptureCheck::Checked => capture["kind"] = value("checked"),
            CaptureCheck::Unchecked { reasons } => {
                capture["kind"] = value("unchecked");
                let mut array = toml_edit::Array::new();
                for reason in reasons {
                    match reason {
                        CaptureCheckReason::ShellStage => array.push("shell_stage"),
                    }
                }
                capture["reasons"] = value(array);
            }
        }
        document["capture_check"] = Item::Table(capture);
        document.to_string().into_bytes()
    }
}
fn hex(bytes: &[u8; 32]) -> String {
    const DIGITS: &[u8] = b"0123456789abcdef";
    let mut result = String::with_capacity(64);
    for byte in bytes {
        result.push(char::from(DIGITS[usize::from(byte >> 4)]));
        result.push(char::from(DIGITS[usize::from(byte & 15)]));
    }
    result
}
fn digest(text: &str) -> Result<[u8; 32], &'static str> {
    if text.len() != 64
        || !text
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err("fingerprint must be 64 lowercase hex digits");
    }
    let mut result = [0; 32];
    for (index, byte) in result.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&text[index * 2..index * 2 + 2], 16)
            .map_err(|_| "invalid fingerprint")?;
    }
    Ok(result)
}
fn valid_timestamp(text: &str) -> bool {
    let b = text.as_bytes();
    if b.len() != 24
        || b[4] != b'-'
        || b[7] != b'-'
        || b[10] != b'T'
        || b[13] != b':'
        || b[16] != b':'
        || b[19] != b'.'
        || b[23] != b'Z'
        || !b
            .iter()
            .enumerate()
            .all(|(i, b)| matches!(i, 4 | 7 | 10 | 13 | 16 | 19 | 23) || b.is_ascii_digit())
    {
        return false;
    }
    let number = |start: usize, end: usize| -> u32 {
        b[start..end]
            .iter()
            .fold(0, |n, b| n * 10 + u32::from(b - b'0'))
    };
    let year = number(0, 4);
    let month = number(5, 7);
    let day = number(8, 10);
    let leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
    let days = match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if leap => 29,
        2 => 28,
        _ => return false,
    };
    (1..=days).contains(&day) && number(11, 13) < 24 && number(14, 16) < 60 && number(17, 19) < 60
}
fn fields(document: &DocumentMut) -> Result<FactRecord, &'static str> {
    let text = |key| {
        document
            .get(key)
            .and_then(Item::as_str)
            .ok_or("missing or non-string fact field")
    };
    let epoch = Epoch::parse(text("epoch")?)?;
    let n = parse_decimal_u64(text("n")?).ok_or("n must be a canonical u64 decimal string")?;
    let job = JobId::parse(text("job")?).map_err(|_| "invalid fact job id")?;
    let finished_at = text("finished_at")?;
    if !valid_timestamp(finished_at) {
        return Err("invalid fact finish timestamp");
    }
    use JobVerdict::*;
    let verdict = [
        BuildFailed,
        WallDeadline,
        OutputBudget,
        Signal,
        NonzeroExit,
        CheckerFailed,
        WrongAnswer,
        Unchecked,
        NoTests,
        Passed,
    ]
    .into_iter()
    .find(|v| v.wire_token() == text("verdict").unwrap_or(""))
    .ok_or("invalid completed test verdict")?;
    let source = digest(text("source")?)?;
    let tests = digest(text("tests")?)?;
    let capture = document
        .get("capture_check")
        .ok_or("capture check missing")?;
    let capture_check = match capture.get("kind").and_then(Item::as_str) {
        Some("checked") => CaptureCheck::Checked,
        Some("unchecked") => {
            let entries = capture
                .get("reasons")
                .and_then(Item::as_array)
                .ok_or("capture reasons missing")?;
            if entries.is_empty() || entries.len() > 4 {
                return Err("invalid capture reason count");
            }
            let mut reasons = Vec::new();
            for entry in entries {
                match entry.as_str() {
                    Some("shell_stage") if !reasons.contains(&CaptureCheckReason::ShellStage) => {
                        reasons.push(CaptureCheckReason::ShellStage)
                    }
                    _ => return Err("invalid capture reason"),
                }
            }
            CaptureCheck::Unchecked { reasons }
        }
        _ => return Err("invalid capture check"),
    };
    Ok(FactRecord {
        epoch,
        n,
        job,
        finished_at: finished_at.into(),
        verdict,
        source,
        tests,
        capture_check,
    })
}

pub fn classify_fact(bytes: &[u8]) -> Record<FactRecord, &'static str> {
    classify_tool_record(bytes, 1, fields)
}
/// The caller checks tool-owned paths through the workspace guard first.
pub fn read_fact(path: impl AsRef<Path>) -> Record<FactRecord, &'static str> {
    read_tool_record(path, 1, fields)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PublicationOutcome {
    Published,
    Superseded,
    NotAttemptedCancelled,
    NotPublished {
        reason: &'static str,
        message: Option<String>,
    },
}
#[derive(Clone, Debug)]
pub struct Publication {
    pub outcome: PublicationOutcome,
    pub durability_uncertain: bool,
    pub cleanup_pending: bool,
    pub recovery_paths: Vec<PathBuf>,
}
impl Publication {
    fn outcome(outcome: PublicationOutcome) -> Self {
        Self {
            outcome,
            durability_uncertain: false,
            cleanup_pending: false,
            recovery_paths: Vec::new(),
        }
    }
    fn failure(reason: &'static str, message: Option<String>) -> Self {
        Self::outcome(PublicationOutcome::NotPublished { reason, message })
    }
}

/// Publish before dropping the job lease. Both reads precede the single
/// cancellation cutoff, even when the first read already rules out publication.
pub fn publish_fact(
    root: &Path,
    problem: &Identifier,
    target: &Identifier,
    fact: &FactRecord,
    cutoff: &mut dyn FnMut() -> bool,
) -> Publication {
    publish_fact_with_options(root, problem, target, fact, cutoff, LockOptions::default())
}

pub fn publish_fact_with_options(
    root: &Path,
    problem: &Identifier,
    target: &Identifier,
    fact: &FactRecord,
    cutoff: &mut dyn FnMut() -> bool,
    options: LockOptions,
) -> Publication {
    let lock = match WorkspaceLock::new(root) {
        Ok(lock) => lock,
        Err(error) => return Publication::failure("io", Some(error.to_string())),
    };
    let guard = match lock.acquire_with_options("publish test fact", options) {
        Ok(guard) => guard,
        Err(error) => {
            let reason = if matches!(error.kind, ErrorKind::Busy { .. }) {
                "lock_busy"
            } else {
                "io"
            };
            return Publication::failure(reason, Some(error.to_string()));
        }
    };
    let sequence = match guard.check_path(".cpforge/job-seq") {
        Ok(path) => read_job_sequence(path),
        Err(error) => Record::Unreadable(std::io::Error::other(error.to_string())),
    };
    let path = fact_path(problem, target);
    let existing = match guard.check_path(&path) {
        Ok(path) => read_fact(path),
        Err(error) => Record::Unreadable(std::io::Error::other(error.to_string())),
    };
    super::crash::barrier("fact.cutoff");
    let mut publication = if cutoff() {
        Publication::outcome(PublicationOutcome::NotAttemptedCancelled)
    } else {
        super::crash::barrier("fact.after_cutoff");
        decide_and_replace(&guard, &path, fact, sequence, existing)
    };
    if let Some(recovery) = guard.recovery_warning() {
        publication.recovery_paths = recovery.paths.clone();
    }
    publication
}
fn decide_and_replace(
    guard: &super::WorkspaceGuard<'_>,
    path: &Path,
    fact: &FactRecord,
    sequence: Record<super::job_seq::JobSequence, &'static str>,
    existing: Record<FactRecord, &'static str>,
) -> Publication {
    let Record::Valid(sequence) = sequence else {
        return Publication::failure("sequence_unavailable", None);
    };
    if sequence.epoch != fact.epoch.as_str() {
        return Publication::failure("epoch_changed", None);
    }
    match existing {
        Record::TooNew { .. } => return Publication::failure("fact_too_new", None),
        Record::Unreadable(error) => {
            return Publication::failure("fact_unreadable", Some(error.to_string()));
        }
        Record::Valid(old) if old.epoch == fact.epoch && old.n >= fact.n => {
            return Publication::outcome(PublicationOutcome::Superseded);
        }
        _ => {}
    }
    if let Err(error) = guard.create_dir_all(path.parent().expect("fact parent")) {
        return Publication::failure("io", Some(error.to_string()));
    }
    super::crash::point("fact.before_replace");
    match atomic_replace(guard, path, &fact.encode(), || Ok(())) {
        Err(error) => Publication::failure("io", Some(error.to_string())),
        Ok(published) => {
            super::crash::point("fact.after_replace");
            super::crash::barrier("fact.after_replace");
            Publication {
                outcome: PublicationOutcome::Published,
                durability_uncertain: matches!(published.durability, Durability::Uncertain(_)),
                cleanup_pending: matches!(published.cleanup, Cleanup::Pending(_)),
                recovery_paths: Vec::new(),
            }
        }
    }
}

#[cfg(test)]
mod tests;
