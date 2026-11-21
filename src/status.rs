//! `cpf status`: lock-free, read-only inspection of the working files and the
//! last completed run (workspace design §3.2, §3.3). Nothing is created,
//! recovered or written; the comparison recomputes both fingerprints over the
//! current files with the publication encoder and never consults history.
use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::fs;
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};

use toml_edit::{DocumentMut, Item};

use crate::config::{self, ConfigRequest, Environment, TargetPlan};
use crate::error::Error;
use crate::exit::ExitCode;
use crate::report::wire::{self, ProblemStatus};
use crate::storage::facts::{
    self,
    fingerprint::{source_fingerprint, tests_fingerprint},
};
use crate::storage::inputs::FrozenFile;
use crate::storage::{self, Record};
use crate::tests_store::{Answer, FrozenAnswer, FrozenTest, TestSettings, TestStore};
use crate::{selection, transport};

pub(crate) fn run(
    reference: Option<&str>,
    all: bool,
    json: bool,
    problem: Option<&str>,
    config_path: Option<PathBuf>,
) -> Result<ExitCode, Error> {
    if !json {
        return inspect(reference, all, false, problem, config_path);
    }
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        inspect(reference, all, true, problem, config_path)
    })) {
        Ok(result) => result,
        Err(payload) => {
            let _ = transport::Delivery::stdout().escaped_panic("status");
            std::panic::resume_unwind(payload)
        }
    }
}

fn inspect(
    reference: Option<&str>,
    all: bool,
    json: bool,
    problem: Option<&str>,
    config_path: Option<PathBuf>,
) -> Result<ExitCode, Error> {
    let mut delivery = transport::Delivery::stdout();
    if json {
        delivery.preflight()?;
    }
    let statuses = match statuses(reference, all, problem, config_path) {
        Ok(statuses) => statuses,
        Err(error) if json => {
            let class = error.class();
            let exit = error.exit_code();
            let bytes = wire::encode_error(&wire::ErrorDocument {
                platform: transport::platform(),
                command: "status".into(),
                class,
                exit: i64::from(exit as u8),
                error: wire::DocumentError {
                    kind: class.token(),
                    message: error.to_string(),
                    path: None,
                },
                diagnostics: Vec::new(),
                diagnostics_dropped: 0,
            })
            .map_err(|e| Error::Internal(e.to_string()))?;
            eprintln!("cpf: error: {error}");
            delivery.document(&bytes)?;
            return Ok(exit);
        }
        Err(error) => return Err(error),
    };
    if json {
        let bytes = wire::encode_status(transport::platform(), &statuses)
            .map_err(|e| Error::Internal(e.to_string()))?;
        delivery.document(&bytes)?;
    } else {
        let mut output = String::new();
        for status in &statuses {
            human(status, &mut output);
        }
        if statuses.is_empty() {
            output.push_str("no problems\n");
        }
        let mut stdout = io::stdout().lock();
        stdout
            .write_all(output.as_bytes())
            .and_then(|()| stdout.flush())
            .map_err(|e| Error::Workspace(format!("cannot write output: {e}")))?;
    }
    Ok(ExitCode::Success)
}

fn statuses(
    reference: Option<&str>,
    all: bool,
    problem: Option<&str>,
    config_path: Option<PathBuf>,
) -> Result<Vec<ProblemStatus>, Error> {
    let cwd = std::env::current_dir().map_err(|e| Error::Workspace(e.to_string()))?;
    let root = crate::workspace_commands::root()?;
    let problems = selection::select_many(&root, &cwd, reference, all, problem)?;
    problems
        .iter()
        .map(|problem| problem_status(&root, &cwd, problem, config_path.clone()))
        .collect()
}

/// Explicit user status (tags, `status`) first and apart from the derived
/// run-fact comparison.
fn human(status: &ProblemStatus, output: &mut String) {
    let _ = writeln!(output, "{}", status.problem);
    if !status.tags.is_empty() {
        let _ = writeln!(output, "  tags: {}", status.tags.join(", "));
    }
    if let Some(user_status) = &status.user_status {
        let _ = writeln!(output, "  status: {user_status}");
    }
    for path in &status.missing {
        let _ = writeln!(output, "  missing registered file: {}", path.display());
    }
    for path in &status.untracked {
        let _ = writeln!(output, "  untracked: {}", path.display());
    }
    let comparison = status.comparison.replace('_', " ");
    let _ = write!(output, "  solution: {comparison}");
    match &status.fact {
        Some(fact) => {
            if let crate::jobs::CaptureCheck::Unchecked { .. } = fact.capture_check {
                output.push_str(" (capture unchecked: shell stage)");
            }
            let _ = writeln!(
                output,
                "; last completed run: {} at {}",
                fact.verdict.wire_token(),
                fact.finished_at
            );
        }
        None => {
            let reason = if status.fact_state == "missing" {
                "no fact".to_owned()
            } else {
                format!("fact {}", status.fact_state.replace('_', " "))
            };
            let _ = writeln!(output, " ({reason})");
        }
    }
    for message in &status.diagnostics {
        let _ = writeln!(output, "  note: {message}");
    }
}

fn problem_status(
    root: &Path,
    cwd: &Path,
    problem: &selection::Problem,
    config_path: Option<PathBuf>,
) -> Result<ProblemStatus, Error> {
    let metadata = storage::read_user_record(problem.dir.join("problem.toml"), 1, |document| {
        Ok::<_, String>(document.clone())
    });
    let document = selection::record_result(metadata).map_err(Error::Workspace)?;
    let fields = document.get("problem");
    let tags = fields
        .and_then(|p| p.get("tags"))
        .and_then(Item::as_array)
        .into_iter()
        .flatten()
        .filter_map(|v| v.as_str().map(str::to_owned))
        .collect();
    let user_status = fields
        .and_then(|p| p.get("status"))
        .and_then(Item::as_str)
        .map(str::to_owned);
    let mut result = ProblemStatus {
        problem: problem.identity.to_string(),
        tags,
        user_status,
        missing: Vec::new(),
        untracked: Vec::new(),
        diagnostics: Vec::new(),
        fact_state: "missing",
        comparison: "unknown",
        fact: None,
    };
    let request = ConfigRequest {
        environment: Environment {
            home: std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" })
                .map(PathBuf::from),
            xdg_config_home: std::env::var_os("XDG_CONFIG_HOME"),
            appdata: std::env::var_os("APPDATA"),
        },
        platform: config::Platform::host(),
        current_dir: cwd.into(),
        config_path,
        workspace_root: Some(root.into()),
        problem_file: Some(problem.dir.join("problem.toml")),
        ..ConfigRequest::default()
    };
    let loaded = config::load_config(&request);
    let target = match loaded.target() {
        Ok(target) => Some(target),
        Err(error) => {
            result
                .diagnostics
                .push(format!("target not fingerprintable: {error}"));
            None
        }
    };
    let patterns = loaded.workspace_ignore().unwrap_or_else(|_| {
        result
            .diagnostics
            .push("ignore patterns unavailable: configuration has errors".into());
        Vec::new()
    });
    let mut registered =
        BTreeSet::from([PathBuf::from("problem.toml"), PathBuf::from("tests.toml")]);
    if let Some(target) = &target {
        for file in target.captured_files().chain(&target.solution_files) {
            registered.insert(file.value.components().iter().collect());
        }
    } else {
        // Explicit declarations still identify registered paths if target
        // resolution fails (for example, an unresolved language).
        for name in ["sources", "support_files", "solution_files"] {
            if let Some(array) = document
                .get("config")
                .and_then(|c| c.get(name))
                .and_then(Item::as_array)
            {
                for value in array {
                    if let Some(path) = value.as_str() {
                        registered.insert(relative(path)?);
                    }
                }
            }
        }
    }
    let mut owned = registered.clone();
    provenance(&document, &mut owned, &mut result.diagnostics);
    let store = TestStore::new(problem.dir.clone());
    let catalog = store.catalog();
    if let Ok(catalog) = &catalog {
        for test in &catalog.entries {
            registered.insert(relative(&test.input)?);
            if let Answer::Named(path) = &test.answer {
                registered.insert(relative(path)?);
            }
        }
    } else if let Err(error) = &catalog {
        result.diagnostics.push(error.clone());
    }
    owned.extend(registered.iter().cloned());
    for path in &registered {
        // Metadata files are structural records, not missing user-file entries.
        if path == Path::new("problem.toml") || path == Path::new("tests.toml") {
            continue;
        }
        let full = problem.dir.join(path);
        match fs::metadata(&full) {
            Ok(metadata) if metadata.is_file() => {}
            Ok(_) => result.diagnostics.push(format!(
                "registered path is not a regular file: {}",
                path.display()
            )),
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::NotFound | io::ErrorKind::NotADirectory
                ) =>
            {
                result.missing.push(path.clone())
            }
            Err(error) => result
                .diagnostics
                .push(format!("{}: {error}", path.display())),
        }
    }
    inventory(
        &problem.dir,
        Path::new(""),
        &owned,
        &patterns
            .iter()
            .map(|p| p.value.as_str())
            .collect::<Vec<_>>(),
        &mut result,
    )?;
    result.untracked.sort();
    let fact_path = facts::fact_path(
        problem.identity.name(),
        &crate::names::Identifier::new(facts::SOLUTION_TARGET).expect("target id"),
    );
    let record = match storage::checked_path(root, &fact_path, false) {
        Ok(path) => facts::read_fact(path),
        Err(error) => Record::Unreadable(io::Error::other(error.to_string())),
    };
    result.fact_state = match record {
        Record::Missing => "missing",
        Record::Unreadable(error) => {
            result.diagnostics.push(format!("fact unreadable: {error}"));
            "unreadable"
        }
        Record::Malformed(error) => {
            result.diagnostics.push(format!("fact malformed: {error}"));
            "malformed"
        }
        Record::TooNew { schema, .. } => {
            result
                .diagnostics
                .push(format!("fact too new: schema {schema}"));
            "too_new"
        }
        Record::Valid(fact) => {
            result.fact = Some(fact);
            "valid"
        }
    };
    if let (Some(fact), Some(target)) = (&result.fact, target.as_ref()) {
        let current = current_fingerprints(&problem.dir, target, &store);
        match current {
            Ok((source, tests)) => {
                result.comparison = match (source != fact.source, tests != fact.tests) {
                    (false, false) => "unchanged",
                    (true, false) => "source_changed",
                    (false, true) => "tests_changed",
                    (true, true) => "source_and_tests_changed",
                }
            }
            Err(error) => result
                .diagnostics
                .push(format!("current inputs not fingerprintable: {error}")),
        }
    }
    Ok(result)
}

fn relative(text: &str) -> Result<PathBuf, Error> {
    if text.is_empty()
        || text.contains(['\\', '\0'])
        || text.split('/').any(|p| {
            matches!(p, "" | "." | "..")
                || Path::new(p).components().count() != 1
                || !matches!(
                    Path::new(p).components().next(),
                    Some(std::path::Component::Normal(_))
                )
        })
    {
        return Err(Error::Workspace(format!(
            "invalid registered path {text:?}"
        )));
    }
    Ok(text.split('/').collect())
}

fn provenance(
    document: &DocumentMut,
    owned: &mut BTreeSet<PathBuf>,
    diagnostics: &mut Vec<String>,
) {
    let Some(files) = document.get("files") else {
        return;
    };
    for key in ["user", "created"] {
        if let Some(array) = files.get(key).and_then(Item::as_array) {
            for value in array {
                let path = if key == "created" {
                    value
                        .as_inline_table()
                        .and_then(|table| table.get("path"))
                        .and_then(toml_edit::Value::as_str)
                } else {
                    value.as_str()
                };
                match path.map(relative) {
                    Some(Ok(path)) => {
                        owned.insert(path);
                    }
                    _ => diagnostics.push(format!("invalid [files] {key} path")),
                }
            }
        }
    }
}

fn inventory(
    dir: &Path,
    relative: &Path,
    owned: &BTreeSet<PathBuf>,
    patterns: &[&str],
    result: &mut ProblemStatus,
) -> Result<(), Error> {
    let entries = fs::read_dir(dir.join(relative)).map_err(|e| Error::Workspace(e.to_string()))?;
    for entry in entries {
        let entry = entry.map_err(|e| Error::Workspace(e.to_string()))?;
        let path = relative.join(entry.file_name());
        let metadata =
            fs::symlink_metadata(entry.path()).map_err(|e| Error::Workspace(e.to_string()))?;
        if storage::is_link(&metadata) || !metadata.is_dir() {
            if !owned.contains(&path)
                && !patterns
                    .iter()
                    .any(|pattern| ignore_matches(pattern, &path))
            {
                result.untracked.push(path);
            }
        } else {
            inventory(dir, &path, owned, patterns, result)?;
        }
    }
    Ok(())
}

pub(crate) fn current_fingerprints(
    dir: &Path,
    target: &TargetPlan,
    store: &TestStore,
) -> Result<([u8; 32], [u8; 32]), String> {
    let files = storage::inputs::current_source_files(target, dir).map_err(|e| e.to_string())?;
    let source = source_fingerprint(&files, target).map_err(|e| e.to_string())?;
    let mut tests = Vec::new();
    for entry in store.catalog()?.entries.into_iter().filter(|e| e.enabled) {
        let input = dir.join(relative(&entry.input).map_err(|e| e.to_string())?);
        let open = |path: &Path| crate::tests_store::open_regular(path).map_err(|e| e.to_string());
        let len = open(&input)?.metadata().map_err(|e| e.to_string())?.len();
        let answer = match entry.answer {
            Answer::Absent => FrozenAnswer::Absent,
            Answer::Named(path) => {
                let path = dir.join(relative(&path).map_err(|e| e.to_string())?);
                let len = open(&path)?.metadata().map_err(|e| e.to_string())?.len();
                FrozenAnswer::Present {
                    path,
                    len,
                    sha256: [0; 32],
                }
            }
        };
        tests.push(FrozenTest {
            id: entry.id,
            origin: entry.origin,
            input: FrozenFile {
                path: input,
                len,
                sha256: [0; 32],
            },
            answer,
            settings: TestSettings::default(),
        });
    }
    let tests = tests_fingerprint(&tests).map_err(|e| e.to_string())?;
    Ok((source, tests))
}

fn units(value: &std::ffi::OsStr) -> Vec<u32> {
    let fold = |unit: u32| {
        if cfg!(any(windows, target_os = "macos"))
            && (u32::from(b'A')..=u32::from(b'Z')).contains(&unit)
        {
            unit + 32
        } else {
            unit
        }
    };
    if let Some(text) = value.to_str() {
        return text.chars().map(|c| fold(u32::from(c))).collect();
    }
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        value
            .as_bytes()
            .iter()
            .map(|b| {
                if b.is_ascii() {
                    fold(u32::from(*b))
                } else {
                    0x11_0000 + u32::from(*b)
                }
            })
            .collect()
    }
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        std::char::decode_utf16(value.encode_wide())
            .map(|c| fold(c.map_or_else(|e| u32::from(e.unpaired_surrogate()), u32::from)))
            .collect()
    }
}

fn component_matches(pattern: &str, name: &std::ffi::OsStr) -> bool {
    let name = units(name);
    let mut matched = vec![false; name.len() + 1];
    matched[0] = true;
    for ch in pattern.chars() {
        let mut next = vec![false; name.len() + 1];
        if ch == '*' {
            next[0] = matched[0];
            for i in 1..=name.len() {
                next[i] = matched[i] || next[i - 1];
            }
        } else {
            let literal = units(std::ffi::OsStr::new(&ch.to_string()))[0];
            for i in 1..=name.len() {
                next[i] = matched[i - 1] && (ch == '?' || literal == name[i - 1]);
            }
        }
        matched = next;
    }
    matched[name.len()]
}

fn ignore_matches(pattern: &str, path: &Path) -> bool {
    let parts: Vec<_> = path.iter().collect();
    if !pattern.contains('/') {
        return parts
            .last()
            .is_some_and(|name| component_matches(pattern, name));
    }
    let mut matched = vec![false; parts.len() + 1];
    matched[0] = true;
    for pattern in pattern.split('/') {
        let mut next = vec![false; parts.len() + 1];
        if pattern == "**" {
            next[0] = matched[0];
            for i in 1..=parts.len() {
                next[i] = matched[i] || next[i - 1];
            }
        } else {
            for i in 1..=parts.len() {
                next[i] = matched[i - 1] && component_matches(pattern, parts[i - 1]);
            }
        }
        matched = next;
    }
    matched[parts.len()]
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn ignore_patterns_match_components_and_unicode_units() {
        for (pattern, parts, expected) in [
            ("*.swp", vec!["nested", "a.swp"], true),
            ("scratch/**", vec!["scratch", "a", "b"], true),
            ("a/**/b", vec!["a", "b"], true),
            ("a/**/b", vec!["a", "x", "b"], true),
            ("a/*/b", vec!["a", "b"], false),
            ("?.txt", vec!["é.txt"], true),
            ("a/b", vec!["x", "a", "b"], false),
            ("A", vec!["a"], cfg!(any(windows, target_os = "macos"))),
        ] {
            let path: PathBuf = parts.into_iter().collect();
            assert_eq!(
                ignore_matches(pattern, &path),
                expected,
                "{pattern}: {path:?}"
            );
        }
    }
}
