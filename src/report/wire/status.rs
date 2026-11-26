//! Schema-1 workspace status. Native paths use lossless report encoding.
use std::path::{Path, PathBuf};

use super::native::{native_bytes, ns};
use super::value::*;
use crate::storage::facts::FactRecord;

#[derive(Debug)]
pub(crate) struct ProblemStatus {
    pub problem: String,
    pub tags: Vec<String>,
    pub user_status: Option<String>,
    pub missing: Vec<PathBuf>,
    pub untracked: Vec<PathBuf>,
    pub diagnostics: Vec<String>,
    pub findings: Vec<crate::organisation::Finding>,
    pub fact_state: &'static str,
    pub comparison: &'static str,
    pub judge: &'static str,
    pub fact: Option<FactRecord>,
}

/// A problem-relative path as `ns` of its native components joined by `/`
/// on every platform (workspace design §3.2 path convention).
fn problem_path(path: &Path) -> Value {
    let mut bytes = Vec::new();
    for component in path.components() {
        if !bytes.is_empty() {
            bytes.push(b'/');
        }
        bytes.extend(native_bytes(component.as_os_str()));
    }
    ns(&bytes)
}

/// Report §6.1 `status` document. Lists are workspace-sized and not capped:
/// the document is never persisted (like `config_show`).
pub(crate) fn encode_status(platform: &str, problems: &[ProblemStatus]) -> Result<Vec<u8>> {
    let mut encoded = Vec::with_capacity(problems.len());
    for problem in problems {
        let capture = match &problem.fact {
            Some(fact) => super::schema::capture_check(&fact.capture_check)?,
            None => Value::Null,
        };
        encoded.push(obj([
            ("problem", s(&problem.problem)),
            ("tags", array(&problem.tags, s)),
            ("user_status", opt(problem.user_status.as_ref(), s)),
            (
                "missing_registered",
                array(&problem.missing, |p| problem_path(p)),
            ),
            ("untracked", array(&problem.untracked, |p| problem_path(p))),
            (
                "targets",
                Value::Array(vec![obj([
                    ("target", ident(crate::storage::facts::SOLUTION_TARGET)?),
                    ("fact_state", token(problem.fact_state)?),
                    ("comparison", token(problem.comparison)?),
                    (
                        "last_verdict",
                        opt(problem.fact.as_ref(), |f| s(f.verdict.wire_token())),
                    ),
                    (
                        "finished_at",
                        opt(problem.fact.as_ref(), |f| s(&f.finished_at)),
                    ),
                    ("capture_check", capture),
                    ("judge", token(problem.judge)?),
                ])]),
            ),
            ("diagnostics", array(&problem.diagnostics, |m| text(m))),
            (
                "findings",
                array(&problem.findings, |finding| {
                    obj([
                        ("field", s(finding.field)),
                        ("reason", text(&finding.reason)),
                    ])
                }),
            ),
        ]));
    }
    Ok(encode(&document(
        "status",
        platform,
        vec![("problems", Value::Array(encoded))],
    )?))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::jobs::{CaptureCheck, CaptureCheckReason};
    use crate::report::JobVerdict;
    use crate::storage::job::{Epoch, JobId};

    #[test]
    fn canonical_status_document_matches_fixture() {
        let fact = FactRecord {
            epoch: Epoch::parse("0123456789abcdef0123456789abcdef").unwrap(),
            n: 3,
            job: JobId::parse("20261120230104123-0000000000000001").unwrap(),
            finished_at: "2026-11-20T23:01:04.123Z".into(),
            verdict: JobVerdict::Passed,
            source: [0; 32],
            tests: [0; 32],
            judge: None,
            capture_check: CaptureCheck::Unchecked {
                reasons: vec![CaptureCheckReason::ShellStage],
            },
        };
        let bytes = encode_status(
            "unix",
            &[
                ProblemStatus {
                    problem: "local:A".into(),
                    tags: Vec::new(),
                    user_status: None,
                    missing: Vec::new(),
                    untracked: Vec::new(),
                    findings: Vec::new(),
                    diagnostics: Vec::new(),
                    fact_state: "missing",
                    comparison: "unknown",
                    judge: "unknown",
                    fact: None,
                },
                ProblemStatus {
                    problem: "local:B".into(),
                    tags: vec!["dp".into()],
                    user_status: Some("solved".into()),
                    missing: vec![PathBuf::from("main.cpp")],
                    untracked: vec![["notes", "a.txt"].iter().collect()],
                    findings: Vec::new(),
                    diagnostics: vec!["note".into()],
                    fact_state: "valid",
                    comparison: "source_changed",
                    judge: "none",
                    fact: Some(fact),
                },
            ],
        )
        .unwrap();
        assert_eq!(bytes, include_bytes!("fixtures/status.json"));
    }
    #[test]
    fn organisation_document_vectors_match_the_encoders() {
        let document: toml_edit::DocumentMut =
            "[problem]\ntags = ['dp', 'DP']\nstatus = 'Working on it'\n"
                .parse()
                .unwrap();
        let problem = ProblemStatus {
            problem: "local:A".into(),
            tags: vec!["dp".into(), "DP".into()],
            user_status: Some("Working on it".into()),
            missing: Vec::new(),
            untracked: Vec::new(),
            diagnostics: Vec::new(),
            findings: crate::organisation::findings(document.get("problem")),
            fact_state: "missing",
            comparison: "unknown",
            judge: "unknown",
            fact: None,
        };
        assert_eq!(
            encode_status("unix", &[problem]).unwrap(),
            include_bytes!("fixtures/status_invalid_fields.json")
        );
        let vectors: serde_json::Value =
            serde_json::from_slice(include_bytes!("fixtures/notes.json")).unwrap();
        for vector in vectors.as_array().unwrap() {
            let bytes = crate::report::wire::encode_notes(
                "unix",
                "local:p",
                Path::new("/workspace/problems/p/notes.md"),
                vector["state"].as_str().unwrap(),
            )
            .unwrap();
            assert_eq!(
                serde_json::from_slice::<serde_json::Value>(&bytes).unwrap(),
                *vector
            );
        }
    }
}
