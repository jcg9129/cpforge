//! Locked inspection and conservative repair of workspace records.
use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::fs;
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};

use toml_edit::{DocumentMut, Item};

use crate::error::Error;
use crate::exit::ExitCode;
use crate::names::{self, Identifier, ProblemIdentity};
use crate::storage::job::{self, ProbeOutcome};
use crate::storage::{self, Record, WorkspaceGuard, WorkspaceLock, facts, job_seq};
use crate::workspace_files as files;

pub(crate) fn run(apply: bool, config_path: Option<PathBuf>) -> Result<ExitCode, Error> {
    let root = crate::workspace_commands::root()?;
    let lock = WorkspaceLock::new(&root)?;
    let guard = lock.acquire("fsck")?;
    crate::workspace_commands::recovery_warnings(&guard);
    let mut inspection = Inspection {
        guard: &guard,
        output: String::new(),
        contests: Vec::new(),
        orphan_facts: Vec::new(),
    };
    inspection.inspect(config_path)?;
    let result = if apply { inspection.apply() } else { Ok(()) };
    let mut stdout = io::stdout().lock();
    stdout
        .write_all(inspection.output.as_bytes())
        .and_then(|()| stdout.flush())
        .map_err(|e| Error::Workspace(format!("cannot write output: {e}")))?;
    result?;
    Ok(ExitCode::Success)
}

struct Inspection<'a, 'g> {
    guard: &'a WorkspaceGuard<'g>,
    output: String,
    contests: Vec<PathBuf>,
    orphan_facts: Vec<(PathBuf, Identifier)>,
}

impl Inspection<'_, '_> {
    fn note(&mut self, path: &Path, message: impl std::fmt::Display) {
        let path = path.strip_prefix(self.guard.root()).unwrap_or(path);
        let _ = writeln!(self.output, "{}: {message}", path.display());
    }

    /// Schema-first classification is displayed even for records we cannot use.
    fn record<T, E: std::fmt::Display>(&mut self, path: &Path, record: Record<T, E>) -> Option<T> {
        match record {
            Record::Valid(value) => {
                self.note(path, "valid (schema 1)");
                Some(value)
            }
            Record::Missing => {
                self.note(path, "missing");
                None
            }
            Record::Unreadable(error) => {
                self.note(path, format!("unreadable: {error}"));
                None
            }
            Record::Malformed(error) => {
                self.note(path, format!("malformed: {error}"));
                None
            }
            Record::TooNew { schema, supported } => {
                self.note(
                    path,
                    format!("too_new: schema {schema}, supported {supported}; kept"),
                );
                None
            }
        }
    }

    fn entries(&mut self, dir: &Path) -> Vec<PathBuf> {
        match entries(self.guard, dir) {
            Ok(entries) => entries,
            Err(error) => {
                self.note(dir, format!("unreadable: {error}"));
                Vec::new()
            }
        }
    }

    fn directory(&mut self, path: &Path) -> bool {
        match fs::symlink_metadata(path) {
            Ok(metadata) if metadata.is_dir() && !storage::is_link(&metadata) => true,
            Ok(_) => {
                self.note(
                    path,
                    "unexpected entry (expected directory, no links); kept",
                );
                false
            }
            Err(error) => {
                self.note(path, format!("unreadable: {error}"));
                false
            }
        }
    }

    fn inspect(&mut self, config_path: Option<PathBuf>) -> Result<(), Error> {
        let root = self.guard.root();
        let marker = root.join("cpforge.toml");
        self.record(
            &marker,
            storage::read_user_record(&marker, 1, |_| Ok::<_, String>(())),
        );
        let metadata = root.join(".cpforge");
        for path in self.entries(&metadata) {
            let name = path.file_name().and_then(|s| s.to_str());
            match name {
                Some("runs" | "cache" | "staging" | "facts") => {
                    self.directory(&path);
                }
                Some("lock") => {} // Never open, read, replace or remove the lock file.
                Some("state.toml" | "job-seq") => {}
                _ => self.note(&path, "unexpected entry in reserved tree; kept"),
            }
        }
        let state = metadata.join("state.toml");
        match self.guard.check_path(&state) {
            Ok(_) => {
                self.record(&state, crate::selection::read_state(&state));
            }
            Err(error) => self.note(&state, format!("unreadable: {error}")),
        }
        let sequence = metadata.join("job-seq");
        match self.guard.check_path(&sequence) {
            Ok(path) => {
                self.record(&path, job_seq::read_job_sequence(&path));
            }
            Err(error) => self.note(&sequence, format!("unreadable: {error}")),
        }
        self.problems(config_path);
        self.contests()?;
        self.facts()?;
        self.runs();
        Ok(())
    }

    fn problems(&mut self, config_path: Option<PathBuf>) {
        let root = self.guard.root();
        let parent = root.join("problems");
        let mut spellings = BTreeMap::new();
        for dir in self.entries(&parent) {
            if !self.directory(&dir) {
                continue;
            }
            if let Some(name) = dir.file_name().and_then(|n| n.to_str()) {
                if let Some(previous) = spellings.insert(names::collision_key(name), dir.clone()) {
                    self.note(
                        &dir,
                        format!("spelling collision with {}", previous.display()),
                    );
                }
            }
            let path = dir.join("problem.toml");
            let record = storage::read_user_record(&path, 1, |document| {
                files::check_problem(document)?;
                Ok::<_, String>(document.clone())
            });
            if matches!(&record, Record::Missing) {
                self.note(&dir, "problem directory without problem.toml; kept");
            }
            let catalog = dir.join("tests.toml");
            self.record(&catalog, crate::tests_store::read_catalog(&catalog));
            let Some(document) = self.record(&path, record) else {
                continue;
            };
            if let Some(id) = document["problem"]["id"].as_str() {
                let identity = ProblemIdentity::parse(id).expect("validated identity");
                if dir.file_name() != Some(std::ffi::OsStr::new(identity.name().as_str())) {
                    self.note(
                        &path,
                        "IdentityMismatch: problem identity does not match directory; kept",
                    );
                }
            }
            let loaded =
                crate::config::load_config(&files::request(root, root, &dir, config_path.clone()));
            let target = loaded.target().ok();
            let registered = match files::registered(&dir, &document, target.as_ref()) {
                Ok(paths) => paths,
                Err(error) => {
                    self.note(&path, format!("registered paths unavailable: {error}"));
                    files::declarations(&document, target.as_ref()).unwrap_or_default()
                }
            };
            let mut diagnostics = Vec::new();
            files::provenance(&dir, &document, &registered, &mut diagnostics);
            for diagnostic in diagnostics {
                self.note(&path, diagnostic);
            }
        }
    }

    fn contests(&mut self) -> Result<(), Error> {
        let parent = self.guard.root().join("contests");
        let mut spellings = BTreeMap::new();
        for dir in self.entries(&parent) {
            if !self.directory(&dir) {
                continue;
            }
            if let Some(name) = dir.file_name().and_then(|name| name.to_str()) {
                if let Some(previous) = spellings.insert(names::collision_key(name), dir.clone()) {
                    self.note(
                        &dir,
                        format!("spelling collision with {}", previous.display()),
                    );
                }
            }
            let path = dir.join("contest.toml");
            let record = storage::read_user_record(&path, 1, |document| {
                crate::add::contest_members(document)?;
                Ok::<_, String>(document.clone())
            });
            let Some(document) = self.record(&path, record) else {
                continue;
            };
            if let Some(name) = dir
                .file_name()
                .and_then(|name| Identifier::from_os_str(name).ok())
            {
                if let Err(error) = crate::selection::contest_binding(&document, &name) {
                    self.note(&path, format!("{error}; kept"));
                    continue;
                }
            } else {
                self.note(&dir, "invalid contest directory name; kept");
                continue;
            }
            let mut dangling = false;
            if let Some(members) = document.get("problem").and_then(Item::as_array_of_tables) {
                for member in members {
                    let id = member["id"].as_str().expect("validated member");
                    let identity = ProblemIdentity::parse(id).expect("validated identity");
                    if deleted(self.guard, identity.name())? {
                        self.note(&path, format!("dangling contest reference: {id}"));
                        dangling = true;
                    }
                }
            }
            if dangling {
                self.contests.push(path);
            }
        }
        Ok(())
    }

    fn facts(&mut self) -> Result<(), Error> {
        let parent = self.guard.root().join(".cpforge").join("facts");
        for dir in self.entries(&parent) {
            if !self.directory(&dir) {
                continue;
            }
            let Some(problem) = dir
                .file_name()
                .and_then(|s| s.to_str())
                .and_then(|s| Identifier::new(s).ok())
            else {
                self.note(&dir, "unexpected fact problem name; kept");
                continue;
            };
            let orphan = deleted(self.guard, &problem)?;
            for path in self.entries(&dir) {
                let target = path
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .and_then(|s| Identifier::new(s).ok());
                if path.extension() != Some(std::ffi::OsStr::new("toml")) || target.is_none() {
                    self.note(&path, "unexpected entry in facts tree; kept");
                    continue;
                }
                let record = match self.guard.check_path(&path) {
                    Ok(_) => facts::read_fact(&path),
                    Err(error) => Record::Unreadable(io::Error::other(error.to_string())),
                };
                let removable = matches!(&record, Record::Valid(_) | Record::Malformed(_));
                self.record(&path, record);
                if orphan {
                    self.note(&path, "fact for deleted problem");
                    if removable {
                        self.orphan_facts.push((path.clone(), problem.clone()));
                    }
                } else if target
                    .as_ref()
                    .is_some_and(|t| t.as_str() != facts::SOLUTION_TARGET)
                {
                    self.note(&path, "fact for unknown target; kept");
                }
            }
        }
        Ok(())
    }

    fn runs(&mut self) {
        let parent = self.guard.root().join(".cpforge").join("runs");
        for dir in self.entries(&parent) {
            if !self.directory(&dir) {
                continue;
            }
            let name = dir.file_name().and_then(|s| s.to_str()).unwrap_or("");
            if name.starts_with(".reclaim-") {
                continue; // Interrupted reclamation is owned by clean/prune.
            }
            if job::JobId::parse(name).is_err() {
                self.note(&dir, "unexpected entry in runs tree; kept");
                continue;
            }
            let path = dir.join("job.toml");
            match self.guard.check_path(&path) {
                Ok(_) => {
                    self.record(&path, job::read_job_record(&path));
                }
                Err(error) => self.note(&path, format!("unreadable: {error}")),
            }
            for path in self.entries(&dir) {
                let name = path.file_name().and_then(|s| s.to_str()).unwrap_or("");
                match name {
                    "lease" | "job.toml" => {
                        if fs::symlink_metadata(&path)
                            .is_ok_and(|m| storage::is_link(&m) || !m.is_file())
                        {
                            self.note(&path, "unexpected entry (expected regular file); kept");
                        }
                    }
                    "report.json" => self.json_schema(&path),
                    "inputs" => self.inputs(&path),
                    "builds" => {
                        if !self.directory(&path) {
                            continue;
                        }
                        for build in self.entries(&path) {
                            if !build
                                .file_name()
                                .and_then(|s| s.to_str())
                                .is_some_and(run_name)
                            {
                                self.note(&build, "unexpected entry in builds directory; kept");
                            } else {
                                self.directory(&build);
                            }
                        }
                    }
                    name if run_name(name) => {
                        if !self.directory(&path) {
                            continue;
                        }
                        for entry in self.entries(&path) {
                            match entry.file_name().and_then(|s| s.to_str()) {
                                Some("spec.json" | "record.json") => self.json_schema(&entry),
                                Some("cleanup.log" | "out" | "cwd") => {}
                                _ => self.note(&entry, "unexpected entry in run directory; kept"),
                            }
                        }
                    }
                    _ => self.note(&path, "unexpected entry in job directory; kept"),
                }
            }
        }
    }

    fn inputs(&mut self, dir: &Path) {
        if !self.directory(dir) {
            return;
        }
        for path in self.entries(dir) {
            match path.file_name().and_then(|s| s.to_str()) {
                Some("src") => {
                    self.directory(&path);
                }
                Some("capture.json") => self.json_schema(&path),
                _ => {
                    let test_file = matches!(
                        path.extension().and_then(|s| s.to_str()),
                        Some("in" | "ans")
                    ) && path
                        .file_stem()
                        .and_then(|s| s.to_str())
                        .is_some_and(|s| Identifier::new(s).is_ok());
                    let regular = fs::symlink_metadata(&path)
                        .is_ok_and(|m| m.is_file() && !storage::is_link(&m));
                    if !(test_file && regular) {
                        self.note(&path, "unexpected entry in inputs directory; kept");
                    }
                }
            }
        }
    }

    fn json_schema(&mut self, path: &Path) {
        if let Err(error) = self.guard.check_path(path) {
            self.note(path, format!("unreadable: {error}"));
            return;
        }
        match storage::read_record_bytes(path) {
            storage::RecordBytes::Missing => self.note(path, "missing"),
            storage::RecordBytes::Unreadable(error) => {
                self.note(path, format!("unreadable: {error}"))
            }
            storage::RecordBytes::TooLarge => {
                self.note(path, "malformed: record exceeds size limit")
            }
            storage::RecordBytes::Present(bytes) => {
                let value = serde_json::from_slice::<serde_json::Value>(&bytes);
                match value {
                    Ok(value) => match value.get("schema").and_then(serde_json::Value::as_u64) {
                        Some(1) => self.note(path, "valid schema 1"),
                        Some(schema) if schema > 1 => {
                            self.note(path, format!("too_new: schema {schema}; kept"))
                        }
                        _ => self.note(path, "malformed schema"),
                    },
                    Err(error) => self.note(path, format!("malformed: {error}")),
                }
            }
        }
    }

    fn apply(&mut self) -> Result<(), Error> {
        // Preflight sequence repair before any other repair; a live or
        // unprobeable job refuses it without partially repairing other records.
        let sequence = self.guard.root().join(".cpforge").join("job-seq");
        let repair_sequence = self.guard.check_path(&sequence).is_ok()
            && matches!(
                job_seq::read_job_sequence(&sequence),
                Record::Missing | Record::Malformed(_)
            );
        if repair_sequence {
            let runs = self.guard.root().join(".cpforge").join("runs");
            let mut blockers = Vec::new();
            for dir in entries(self.guard, &runs)? {
                let metadata =
                    fs::symlink_metadata(&dir).map_err(|e| storage::StorageError::io(&dir, e))?;
                if storage::is_link(&metadata) {
                    blockers.push(format!("{}: link cannot be probed", dir.display()));
                } else if metadata.is_dir() {
                    match job::probe(&dir) {
                        ProbeOutcome::Held => {
                            blockers.push(format!("{}: lease held", dir.display()))
                        }
                        ProbeOutcome::Failed(error) => blockers.push(format!(
                            "{}: lease cannot be probed: {error}",
                            dir.display()
                        )),
                        // The probe lock is released as soon as the arm ends.
                        ProbeOutcome::Acquired(_) | ProbeOutcome::Absent => {}
                    }
                }
            }
            if !blockers.is_empty() {
                return Err(Error::Workspace(format!(
                    "cannot repair job-seq; jobs may be live: {}",
                    blockers.join("; ")
                )));
            }
        }
        let mut failures = Vec::new();
        for path in self.contests.clone() {
            let check = |document: &DocumentMut| {
                files::check_schema(document)?;
                crate::add::contest_members(document)?;
                let name = path
                    .parent()
                    .and_then(Path::file_name)
                    .and_then(|name| Identifier::from_os_str(name).ok())
                    .ok_or("invalid contest directory name")?;
                crate::selection::contest_binding(document, &name)
            };
            let result = storage::edit_toml(self.guard, &path, check, |document| {
                if let Some(members) = document
                    .get_mut("problem")
                    .and_then(Item::as_array_of_tables_mut)
                {
                    // Check deletion again against the fresh record, preserving
                    // membership order and all untouched labels/unknown fields.
                    let mut remove = Vec::new();
                    for (index, member) in members.iter().enumerate() {
                        let identity = ProblemIdentity::parse(
                            member["id"].as_str().expect("validated member"),
                        )
                        .map_err(|e| e.to_string())?;
                        if deleted(self.guard, identity.name()).map_err(|e| e.to_string())? {
                            remove.push(index);
                        }
                    }
                    for index in remove.into_iter().rev() {
                        members.remove(index);
                    }
                }
                Ok::<_, String>(())
            });
            match result {
                Ok(published) => {
                    crate::workspace_commands::publication_warnings(&path, &published);
                    self.note(&path, "removed dangling contest references");
                }
                Err(error) => failures.push(format!("{}: {error}", path.display())),
            }
        }
        for (path, problem) in self.orphan_facts.clone() {
            if let Err(error) = self.remove_orphan_fact(&path, &problem) {
                failures.push(error.to_string());
            }
        }
        if repair_sequence {
            let mut random = [0; 16];
            getrandom::fill(&mut random).map_err(|e| Error::Workspace(e.to_string()))?;
            let epoch = format!("{:032x}", u128::from_le_bytes(random));
            let bytes = format!("schema = 1\nepoch = \"{epoch}\"\nnext = \"0\"\n");
            match storage::atomic_replace(self.guard, &sequence, bytes.as_bytes(), || Ok(())) {
                Ok(published) => {
                    storage::crash::point("fsck.sequence_replaced");
                    crate::workspace_commands::publication_warnings(&sequence, &published);
                    self.note(&sequence, "repaired with fresh epoch, next = 0");
                }
                Err(error) => failures.push(error.to_string()),
            }
        }
        if failures.is_empty() {
            Ok(())
        } else {
            Err(Error::Workspace(failures.join("; ")))
        }
    }

    /// Recheck deletion and classification under the lock before removing.
    fn remove_orphan_fact(&mut self, path: &Path, problem: &Identifier) -> Result<(), Error> {
        self.guard.check_path(path)?;
        if deleted(self.guard, problem)?
            && matches!(
                facts::read_fact(path),
                Record::Valid(_) | Record::Malformed(_)
            )
        {
            fs::remove_file(path).map_err(|e| storage::StorageError::io(path, e))?;
            storage::crash::point("fsck.fact_removed");
            self.note(path, "removed orphan fact");
        }
        Ok(())
    }
}

fn entries(guard: &WorkspaceGuard<'_>, dir: &Path) -> Result<Vec<PathBuf>, Error> {
    guard.check_path(dir)?;
    let listing = match fs::read_dir(dir) {
        Ok(listing) => listing,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(storage::StorageError::io(dir, error).into()),
    };
    let mut paths = Vec::new();
    for entry in listing {
        paths.push(entry.map_err(|e| storage::StorageError::io(dir, e))?.path());
    }
    paths.sort();
    Ok(paths)
}

/// Only absence establishes deletion. Broken metadata, links, spelling
/// collisions and newer schemas cannot authorize deleting references or facts.
fn deleted(guard: &WorkspaceGuard<'_>, name: &Identifier) -> Result<bool, Error> {
    let (resolution, _) = crate::selection::inspect_problem(guard.root(), name)?;
    Ok(matches!(resolution, names::Resolution::Create))
}

fn run_name(name: &str) -> bool {
    name.len() == 6 && name.starts_with('r') && name.as_bytes()[1..].iter().all(u8::is_ascii_digit)
}
