//! Local problem publication and replayable contest membership.

use std::collections::BTreeSet;
use std::fs::{self, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

use toml_edit::{ArrayOfTables, DocumentMut, Item, Table, value};

use crate::config::{ConfigRequest, Environment, Operation, Severity, load_config};
use crate::error::Error;
use crate::names::{
    self, EntryKind, Identifier, ProblemEntry, ProblemIdentity, ProblemMetadata, Resolution,
};
use crate::storage::{
    self, Cleanup, ErrorKind, Record, StorageError, WorkspaceGuard, WorkspaceLock, crash,
};
use crate::workspace::{Report, cleanup_warning, publication_warnings};

pub(crate) fn add(
    name: &str,
    contest: Option<&str>,
    label: Option<&str>,
    language: Option<&str>,
) -> Result<(), Error> {
    let name = Identifier::new(name)
        .map_err(|error| Error::Failure(format!("invalid problem name: {error}")))?;
    let contest = contest
        .map(Identifier::new)
        .transpose()
        .map_err(|error| Error::Failure(format!("invalid contest id: {error}")))?;
    if let Some(label) = label {
        validate_label(label).map_err(Error::Failure)?;
    }
    let current = std::env::current_dir()
        .map_err(|error| Error::Failure(format!("cannot read current directory: {error}")))?;
    let root = find_root(&current)?;
    let lock = WorkspaceLock::new(root)?;
    let guard = lock.acquire("add")?;
    let mut report = Report::default();
    if let Some(warning) = guard.recovery_warning() {
        for (path, error) in warning.paths.iter().zip(&warning.errors) {
            report.warnings.push(format!(
                "leftover recovery incomplete at {}: {error}",
                path.display()
            ));
        }
    }
    let result = add_locked(
        &guard,
        &name,
        contest.as_ref(),
        label,
        language,
        &mut report,
    );
    for line in report.lines {
        println!("{line}");
    }
    for warning in report.warnings {
        eprintln!("cpf: warning: {warning}");
    }
    result
}

fn find_root(current: &Path) -> Result<PathBuf, Error> {
    for ancestor in current.ancestors() {
        let marker = ancestor.join("cpforge.toml");
        match fs::metadata(&marker) {
            Ok(metadata) if metadata.is_file() => return Ok(ancestor.to_path_buf()),
            Ok(_) => {}
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::NotFound | io::ErrorKind::NotADirectory
                ) => {}
            Err(error) => return Err(StorageError::io(marker, error).into()),
        }
    }
    Err(Error::Failure(
        "outside a workspace: no cpforge.toml found in this directory or its ancestors".into(),
    ))
}

fn add_locked(
    guard: &WorkspaceGuard<'_>,
    name: &Identifier,
    contest: Option<&Identifier>,
    label: Option<&str>,
    language: Option<&str>,
    report: &mut Report,
) -> Result<(), Error> {
    crash::point("add.locked");
    let created = if problem_exists(guard, name)? {
        false
    } else {
        create_problem(guard, name, language, report)?
    };
    let relative = format!("problems/{}", name.as_str());
    if created {
        report.lines.push(format!("created {relative}/"));
    } else {
        report
            .lines
            .push(format!("already exists; kept {relative}/"));
    }
    if let Some(contest) = contest {
        let identity = ProblemIdentity::local(name.clone()).to_string();
        if let Err(error) = add_membership(guard, contest, &identity, label, report) {
            return Err(Error::Failure(format!(
                "problem {}; contest membership failed: {error}",
                if created { "created" } else { "already exists" }
            )));
        }
    }
    Ok(())
}

fn identity(document: &DocumentMut) -> Result<String, String> {
    let id = document
        .get("problem")
        .and_then(|item| item.get("id"))
        .and_then(Item::as_str)
        .ok_or_else(|| "[problem] id must be a string".to_owned())?;
    ProblemIdentity::parse(id).map_err(|error| error.to_string())?;
    Ok(id.to_owned())
}

fn record_result<T, E: std::fmt::Display>(record: Record<T, E>) -> Result<T, String> {
    match record {
        Record::Valid(value) => Ok(value),
        Record::Missing => Err("missing".into()),
        Record::Unreadable(error) => Err(format!("unreadable: {error}")),
        Record::Malformed(error) => Err(format!("malformed: {error}")),
        Record::TooNew { schema, supported } => {
            Err(format!("too_new: schema {schema}, supported {supported}"))
        }
    }
}

fn problem_exists(guard: &WorkspaceGuard<'_>, name: &Identifier) -> Result<bool, Error> {
    let directory = guard.check_path("problems")?;
    let listing = match fs::read_dir(&directory) {
        Ok(listing) => listing,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(StorageError::io(directory, error).into()),
    };
    let mut entries: Vec<ProblemEntry> = Vec::new();
    for entry in listing {
        let entry = entry.map_err(|error| StorageError::io(&directory, error))?;
        let entry_name = entry.file_name();
        if names::collides_with_native(name.as_str(), &entry_name) {
            entries.push(ProblemEntry {
                name: entry_name,
                kind: EntryKind::Other,
                metadata: ProblemMetadata::Missing,
            });
        }
    }
    let mut detail = String::new();
    // Several entries and different spellings take precedence over record I/O.
    if let [entry] = entries.as_mut_slice() {
        if entry.name == std::ffi::OsStr::new(name.as_str()) {
            let path = directory.join(&entry.name);
            let metadata =
                fs::symlink_metadata(&path).map_err(|error| StorageError::io(&path, error))?;
            entry.kind = if storage::is_link(&metadata) {
                EntryKind::Link
            } else if metadata.is_dir() {
                EntryKind::Directory
            } else {
                EntryKind::Other
            };
            if entry.kind == EntryKind::Directory {
                let record = storage::read_user_record(path.join("problem.toml"), 1, identity);
                entry.metadata = match record {
                    Record::Valid(identity) => ProblemMetadata::Valid { identity },
                    other => {
                        let classification = match &other {
                            Record::Missing => ProblemMetadata::Missing,
                            Record::Unreadable(_) => ProblemMetadata::Unreadable,
                            Record::Malformed(_) => ProblemMetadata::Malformed,
                            Record::TooNew { .. } => ProblemMetadata::TooNew,
                            Record::Valid(_) => unreachable!(),
                        };
                        detail = format!(
                            "; {}: {}",
                            path.join("problem.toml").display(),
                            record_result(other).unwrap_err()
                        );
                        classification
                    }
                };
            }
        }
    }
    match names::resolve_add(name, &entries) {
        Resolution::Create => Ok(false),
        Resolution::Exists => Ok(true),
        Resolution::NameCollision { reason, entries } => {
            let entries = entries
                .iter()
                .map(|entry| directory.join(entry).display().to_string())
                .collect::<Vec<String>>()
                .join(", ");
            Err(Error::Failure(format!(
                "name collision with {entries}: {reason:?}{detail}"
            )))
        }
    }
}

fn configuration_error(diagnostics: &[crate::config::Diagnostic]) -> Error {
    // Each diagnostic already names its origin and severity.
    let mut message = String::from("configuration failed");
    for diagnostic in diagnostics {
        message.push_str("\n  ");
        message.push_str(&diagnostic.to_string());
    }
    Error::Failure(message)
}

fn create_problem(
    guard: &WorkspaceGuard<'_>,
    name: &Identifier,
    language: Option<&str>,
    report: &mut Report,
) -> Result<bool, Error> {
    let loaded = load_config(&ConfigRequest {
        environment: Environment {
            home: std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" })
                .map(PathBuf::from),
            xdg_config_home: std::env::var_os("XDG_CONFIG_HOME"),
            appdata: std::env::var_os("APPDATA"),
        },
        current_dir: guard.root().to_path_buf(),
        workspace_root: Some(guard.root().to_path_buf()),
        problem_file: Some(
            guard
                .root()
                .join("problems")
                .join(name.as_str())
                .join("problem.toml"),
        ),
        prospective_problem: Some(problem_record(name, language, None)),
        operation: Operation::Other,
        ..ConfigRequest::default()
    });
    if loaded.has_errors() {
        return Err(configuration_error(loaded.diagnostics()));
    }
    let unresolved = language.is_none() && loaded.merged().default_language().is_none();
    let target = if unresolved {
        None
    } else {
        Some(
            loaded
                .target()
                .map_err(|error| configuration_error(&error.diagnostics))?,
        )
    };
    for diagnostic in loaded
        .diagnostics()
        .iter()
        .filter(|diagnostic| diagnostic.severity == Severity::Warning)
    {
        report.warnings.push(diagnostic.to_string());
    }
    let template = if let Some(target) = target {
        target
            .template_destination()
            .map_err(|error| Error::Failure(error.to_string()))?
            .map(|(template, destination)| {
                let destination = solution_destination(destination.value.components())?;
                let bytes = read_template(&template.value)?;
                Ok::<_, Error>((destination, bytes))
            })
            .transpose()?
    } else {
        None
    };
    let created = publish_problem(
        guard,
        name,
        problem_record(
            name,
            language,
            template.as_ref().map(|(path, _)| path.as_str()),
        )
        .as_bytes(),
        template.as_ref(),
        report,
    )?;
    if let (true, Some((destination, _))) = (created, &template) {
        report
            .lines
            .push(format!("created problems/{}/{destination}", name.as_str()));
    }
    if created && template.is_none() {
        report.lines.push(if unresolved {
            "language unresolved; created problem without a solution file".into()
        } else {
            "no template; created problem without a solution file".into()
        });
    }
    Ok(created)
}

/// The copied solution is a tool-written path: every component must be a
/// portable component (storage §2, §2.1), and it may not replace the record.
fn solution_destination(components: &[String]) -> Result<String, Error> {
    let path = components.join("/");
    for component in components {
        names::PortableComponent::new(component.as_str()).map_err(|error| {
            Error::Failure(format!(
                "template destination {path} is not portable: {component:?}: {error}"
            ))
        })?;
    }
    if components.len() == 1
        && names::collides_with_native("problem.toml", std::ffi::OsStr::new(&components[0]))
    {
        return Err(Error::Failure(format!(
            "template destination {path} would replace problem.toml"
        )));
    }
    Ok(path)
}

/// Render the staged `problem.toml` with standard (non-inline) tables.
fn problem_record(name: &Identifier, language: Option<&str>, solution: Option<&str>) -> String {
    // Indexing a missing key with toml_edit creates an inline table, so every
    // section is built as an explicit table with a blank line before its header.
    fn section(document: &mut DocumentMut, key: &str, fill: impl FnOnce(&mut Table)) {
        let mut table = Table::new();
        table.decor_mut().set_prefix("\n");
        fill(&mut table);
        document.insert(key, Item::Table(table));
    }
    let mut document = DocumentMut::new();
    document.insert("schema", value(1));
    section(&mut document, "problem", |table| {
        table.insert(
            "id",
            value(ProblemIdentity::local(name.clone()).to_string()),
        );
    });
    if let Some(language) = language {
        section(&mut document, "config", |table| {
            table.insert("language", value(language));
        });
    }
    if let Some(path) = solution {
        let mut entry = toml_edit::InlineTable::new();
        entry.insert("path", path.into());
        entry.insert("command", "add".into());
        let mut created = toml_edit::Array::new();
        created.push(entry);
        section(&mut document, "files", |table| {
            table.insert("created", value(created));
        });
    }
    document.to_string()
}

fn publish_problem(
    guard: &WorkspaceGuard<'_>,
    name: &Identifier,
    metadata: &[u8],
    template: Option<&(String, Vec<u8>)>,
    report: &mut Report,
) -> Result<bool, Error> {
    guard.create_dir_all(".cpforge/staging")?;
    let stage = loop {
        let mut bits = [0_u8; 16];
        getrandom::fill(&mut bits)
            .map_err(|error| Error::Failure(format!("cannot generate staging id: {error}")))?;
        let path = guard.check_path(format!(
            ".cpforge/staging/{:032x}",
            u128::from_le_bytes(bits)
        ))?;
        match fs::create_dir(&path) {
            Ok(()) => break path,
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(StorageError::io(path, error).into()),
        }
    };
    let mut publication_attempted = false;
    let result: Result<bool, Error> = (|| {
        write_staged(guard, &stage.join("problem.toml"), metadata)?;
        if let Some((destination, bytes)) = template {
            write_staged(guard, &stage.join(destination), bytes)?;
        }
        crash::point("add.staged");
        guard.create_dir_all("problems")?;
        let target = guard.check_path(format!("problems/{}", name.as_str()))?;
        publication_attempted = true;
        match storage::publish_dir_noclobber(guard, &stage, &target) {
            Ok(published) => {
                publication_warnings(report, &target, &published);
                crash::point("add.published");
                Ok(true)
            }
            Err(mut error) => {
                // This command owns the staged tree and removes it on every
                // pre-commit failure, rather than waiting for the next recovery.
                clean_stage(guard, &stage, report);
                // Cleanup failures are already retained in the command report.
                error.cleanup = Cleanup::Done;
                if matches!(&error.kind, ErrorKind::Exists { .. }) {
                    if problem_exists(guard, name)? {
                        return Ok(false);
                    }
                    return Err(Error::Failure(format!(
                        "name collision: native destination exists for {}: {error}",
                        name.as_str()
                    )));
                }
                Err(error.into())
            }
        }
    })();
    if result.is_err() && !publication_attempted {
        clean_stage(guard, &stage, report);
    }
    result
}

fn write_staged(guard: &WorkspaceGuard<'_>, path: &Path, bytes: &[u8]) -> Result<(), Error> {
    guard.create_dir_all(path.parent().expect("staged file parent"))?;
    let path = guard.check_path(path)?;
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
        .map_err(|error| StorageError::io(&path, error))?;
    file.write_all(bytes)
        .map_err(|error| StorageError::io(path, error))?;
    Ok(())
}

fn clean_stage(guard: &WorkspaceGuard<'_>, stage: &Path, report: &mut Report) {
    match storage::remove_tree(guard, stage) {
        Ok(walk) if walk.errors.is_empty() && walk.skipped.is_empty() => {}
        Ok(walk) => {
            for (path, error) in walk.errors {
                report
                    .warnings
                    .push(format!("cleanup pending: {}: {error}", path.display()));
            }
            for skipped in walk.skipped {
                report.warnings.push(format!(
                    "cleanup pending: {}: {:?}",
                    skipped.path.display(),
                    skipped.reason
                ));
            }
        }
        Err(error) => {
            report.warnings.push(format!("cleanup pending: {error}"));
        }
    }
}

/// The template lives outside the workspace and is only read, so links are
/// followed (storage contract §2); it must resolve to a regular file.
fn read_template(path: &Path) -> Result<Vec<u8>, Error> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        // Never block on a FIFO named as the template.
        options.custom_flags(libc::O_NONBLOCK | libc::O_CLOEXEC);
    }
    let mut file = options
        .open(path)
        .map_err(|error| StorageError::io(path, error))?;
    let metadata = file
        .metadata()
        .map_err(|error| StorageError::io(path, error))?;
    if !metadata.is_file() {
        return Err(Error::Failure(format!(
            "template {} is not a regular file",
            path.display()
        )));
    }
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)
        .map_err(|error| StorageError::io(path, error))?;
    Ok(bytes)
}

fn validate_label(label: &str) -> Result<(), String> {
    let valid = (1..=16).contains(&label.len())
        && label
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'));
    if valid {
        Ok(())
    } else {
        Err("label must be 1-16 ASCII letters, digits, `-` or `_`".into())
    }
}

fn contest_members(document: &DocumentMut) -> Result<(), String> {
    let Some(item) = document.get("problem") else {
        return Ok(());
    };
    let records = item
        .as_array_of_tables()
        .ok_or("problem must be an array of records ([[problem]])")?;
    let mut ids: BTreeSet<String> = BTreeSet::new();
    let mut labels: BTreeSet<String> = BTreeSet::new();
    for record in records {
        let id = record
            .get("id")
            .and_then(Item::as_str)
            .ok_or("member id must be a string")?;
        ProblemIdentity::parse(id).map_err(|error| error.to_string())?;
        if !ids.insert(id.to_owned()) {
            return Err(format!("duplicate member id {id:?}"));
        }
        if let Some(label) = record.get("label") {
            let label = label.as_str().ok_or("member label must be a string")?;
            validate_label(label)?;
            if !labels.insert(label.to_owned()) {
                return Err(format!("duplicate member label {label:?}"));
            }
        }
    }
    Ok(())
}

fn check_contest(document: &DocumentMut) -> Result<(), String> {
    // The edit primitive has already bounded and parsed this fresh baseline.
    // Check the schema directly so rendering cannot change the byte-size check.
    if let Some(item) = document.get("schema") {
        let schema = item
            .as_integer()
            .ok_or("malformed: `schema` is not an integer")?;
        if schema > 1 {
            return Err(format!("too_new: schema {schema}, supported 1"));
        }
        if schema < 1 {
            return Err(format!(
                "malformed: schema {schema} is older than supported 1"
            ));
        }
    }
    contest_members(document).map_err(|reason| format!("malformed: {reason}"))
}

fn append_member(document: &mut DocumentMut, id: &str, label: Option<&str>) -> Result<(), String> {
    if let Some(records) = document.get("problem").and_then(Item::as_array_of_tables) {
        if let Some(record) = records
            .iter()
            .find(|record| record.get("id").and_then(Item::as_str) == Some(id))
        {
            let existing = record.get("label").and_then(Item::as_str);
            if label.is_some() && existing != label {
                return Err(format!(
                    "label conflict for {id}: existing {existing:?}, requested {label:?}"
                ));
            }
            return Ok(());
        }
        if let Some(label) = label {
            if records
                .iter()
                .any(|record| record.get("label").and_then(Item::as_str) == Some(label))
            {
                return Err(format!(
                    "label conflict: {label:?} already belongs to another problem"
                ));
            }
        }
    } else {
        document["problem"] = Item::ArrayOfTables(ArrayOfTables::new());
    }
    let mut record = Table::new();
    record.decor_mut().set_prefix("\n");
    record["id"] = value(id);
    if let Some(label) = label {
        record["label"] = value(label);
    }
    document["problem"]
        .as_array_of_tables_mut()
        .expect("checked contest members")
        .push(record);
    Ok(())
}

fn add_membership(
    guard: &WorkspaceGuard<'_>,
    contest: &Identifier,
    id: &str,
    label: Option<&str>,
    report: &mut Report,
) -> Result<(), Error> {
    let contests = guard.create_dir_all("contests")?;
    for entry in fs::read_dir(&contests).map_err(|error| StorageError::io(&contests, error))? {
        let entry = entry.map_err(|error| StorageError::io(&contests, error))?;
        if names::collides_with_native(contest.as_str(), &entry.file_name())
            && entry.file_name() != std::ffi::OsStr::new(contest.as_str())
        {
            return Err(Error::Failure(format!(
                "contest name collision with {}",
                entry.path().display()
            )));
        }
    }
    let directory = guard.create_dir_all(contests.join(contest.as_str()))?;
    let path = guard.check_path(directory.join("contest.toml"))?;
    let mut record = storage::read_user_record(&path, 1, contest_members);
    if matches!(record, Record::Missing) {
        match storage::publish_file_noclobber(guard, &path, b"schema = 1\n") {
            Ok(published) => {
                publication_warnings(report, &path, &published);
                report.lines.push(format!(
                    "created contests/{}/contest.toml",
                    contest.as_str()
                ));
                crash::point("add.contest_created");
            }
            Err(error) if matches!(&error.kind, ErrorKind::Exists { .. }) => {
                cleanup_warning(report, &error.cleanup);
            }
            Err(error) => return Err(error.into()),
        }
        guard.check_path(&path)?;
        record = storage::read_user_record(&path, 1, contest_members);
    }
    record_result(record)
        .map_err(|reason| Error::Failure(format!("{} is {reason}", path.display())))?;
    let published = storage::edit_toml_replay(guard, &path, check_contest, |document| {
        append_member(document, id, label)
    })
    .map_err(|error| Error::Failure(format!("{}: {error}", path.display())))?;
    publication_warnings(report, &path, &published);
    crash::point("add.member_added");
    report.lines.push(format!(
        "{} {id} in contest {}",
        if published.committed { "added" } else { "kept" },
        contest.as_str()
    ));
    Ok(())
}

#[cfg(all(test, feature = "storage-test-hooks"))]
mod tests {
    use super::*;
    use crate::storage::{LockOptions, TestHooks};

    #[derive(Debug)]
    struct Directory(PathBuf);

    impl Directory {
        fn new() -> Self {
            let mut bits = [0_u8; 16];
            getrandom::fill(&mut bits).unwrap();
            let path = std::env::temp_dir().join(format!(
                "cpforge-add-storage-{:032x}",
                u128::from_le_bytes(bits)
            ));
            fs::create_dir(&path).unwrap();
            Self(fs::canonicalize(path).unwrap())
        }
    }

    impl Drop for Directory {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    const METADATA: &[u8] = b"schema = 1\n\n[problem]\nid = \"local:one\"\n";

    #[test]
    fn failed_directory_publications_remove_staging_without_publishing() {
        for hooks in [
            TestHooks {
                fail_pre_commit_sync: true,
                ..TestHooks::default()
            },
            TestHooks {
                force_directory_unsupported: true,
                ..TestHooks::default()
            },
            TestHooks {
                force_cross_filesystem: true,
                ..TestHooks::default()
            },
            TestHooks {
                fail_publish_permission_denied: true,
                ..TestHooks::default()
            },
        ] {
            let dir = Directory::new();
            let lock = WorkspaceLock::new(&dir.0).unwrap();
            let guard = lock
                .acquire_with_options(
                    "add",
                    LockOptions {
                        test_hooks: hooks,
                        ..LockOptions::default()
                    },
                )
                .unwrap();
            let mut report = Report::default();
            let name = Identifier::new("one").unwrap();
            publish_problem(&guard, &name, METADATA, None, &mut report).unwrap_err();
            assert!(!dir.0.join("problems/one").exists());
            assert_eq!(
                fs::read_dir(dir.0.join(".cpforge/staging"))
                    .unwrap()
                    .count(),
                0
            );
            assert!(report.warnings.is_empty());
        }
    }

    #[test]
    fn committed_publication_warns_once_and_membership_still_completes() {
        let dir = Directory::new();
        let lock = WorkspaceLock::new(&dir.0).unwrap();
        let guard = lock
            .acquire_with_options(
                "add",
                LockOptions {
                    test_hooks: TestHooks {
                        post_commit_sync_failure: true,
                        ..TestHooks::default()
                    },
                    ..LockOptions::default()
                },
            )
            .unwrap();
        let mut report = Report::default();
        assert!(
            publish_problem(
                &guard,
                &Identifier::new("one").unwrap(),
                METADATA,
                None,
                &mut report
            )
            .unwrap()
        );
        assert_eq!(report.warnings.len(), 1);
        add_membership(
            &guard,
            &Identifier::new("round").unwrap(),
            "local:one",
            Some("A"),
            &mut report,
        )
        .unwrap();
        assert_eq!(report.warnings.len(), 3);
        assert!(
            report
                .warnings
                .iter()
                .all(|warning| warning.contains("durability uncertain"))
        );
        assert_eq!(
            fs::read(dir.0.join("problems/one/problem.toml")).unwrap(),
            METADATA
        );
        let bytes = fs::read(dir.0.join("contests/round/contest.toml")).unwrap();
        add_membership(
            &guard,
            &Identifier::new("round").unwrap(),
            "local:one",
            Some("A"),
            &mut report,
        )
        .unwrap();
        assert_eq!(report.warnings.len(), 3);
        assert_eq!(
            fs::read(dir.0.join("contests/round/contest.toml")).unwrap(),
            bytes
        );
    }

    #[test]
    fn directory_publication_exists_is_reclassified_before_reuse() {
        for (bytes, message) in [
            (METADATA, None),
            (b"schema = 99\n".as_slice(), Some("too_new")),
            (b"schema = [".as_slice(), Some("malformed")),
            (
                b"[problem]\nid = 'local:other'\n".as_slice(),
                Some("IdentityMismatch"),
            ),
        ] {
            let dir = Directory::new();
            let lock = WorkspaceLock::new(&dir.0).unwrap();
            let guard = lock.acquire("add").unwrap();
            let name = Identifier::new("one").unwrap();
            assert!(!problem_exists(&guard, &name).unwrap());
            // An outside creator publishes between the lexical check and rename.
            guard.create_dir_all("problems/one").unwrap();
            fs::write(dir.0.join("problems/one/problem.toml"), bytes).unwrap();
            fs::write(dir.0.join("problems/one/keep"), b"protected").unwrap();
            let mut report = Report::default();
            let result = publish_problem(&guard, &name, METADATA, None, &mut report);
            if let Some(message) = message {
                assert!(result.unwrap_err().to_string().contains(message));
            } else {
                assert!(!result.unwrap());
            }
            assert_eq!(
                fs::read(dir.0.join("problems/one/problem.toml")).unwrap(),
                bytes
            );
            assert_eq!(
                fs::read(dir.0.join("problems/one/keep")).unwrap(),
                b"protected"
            );
            assert_eq!(
                fs::read_dir(dir.0.join(".cpforge/staging"))
                    .unwrap()
                    .count(),
                0
            );
        }
    }
}
