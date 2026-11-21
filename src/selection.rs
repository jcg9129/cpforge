//! Problem lookup, invocation selection, and the saved workspace selection.

use std::ffi::{OsStr, OsString};
use std::fmt;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use toml_edit::{DocumentMut, Item};

use crate::config::ConfigRequest;
use crate::error::Error;
use crate::names::{
    self, CollisionReason, EntryKind, Identifier, ProblemEntry, ProblemIdentity, ProblemMetadata,
    Resolution,
};
use crate::storage::{self, Record, StorageError, WorkspaceGuard, WorkspaceLock};
use crate::workspace::{Report, find_root, publication_warnings};

fn problem_identity(document: &DocumentMut) -> Result<String, String> {
    let id = document
        .get("problem")
        .and_then(|item| item.get("id"))
        .and_then(Item::as_str)
        .ok_or_else(|| "[problem] id must be a string".to_owned())?;
    ProblemIdentity::parse(id).map_err(|error| error.to_string())?;
    Ok(id.to_owned())
}

pub(crate) fn record_result<T, E: std::fmt::Display>(record: Record<T, E>) -> Result<T, String> {
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

pub(crate) fn inspect_problem(
    root: &Path,
    name: &Identifier,
) -> Result<(Resolution, String), StorageError> {
    let directory = storage::checked_path(root, Path::new("problems"), false)?;
    let listing = match fs::read_dir(&directory) {
        Ok(listing) => listing,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Ok((Resolution::Create, String::new()));
        }
        Err(error) => return Err(StorageError::io(directory, error)),
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
                let record =
                    storage::read_user_record(path.join("problem.toml"), 1, problem_identity);
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
    Ok((names::resolve_add(name, &entries), detail))
}

/// A validated existing problem. Its identity is bound to its directory spelling.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Problem {
    pub dir: PathBuf,
    pub identity: ProblemIdentity,
}

/// The winning source; an invalid winner never falls through to another source.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    Flag,
    Cwd,
    Saved,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Selected {
    pub root: PathBuf,
    pub dir: PathBuf,
    pub identity: ProblemIdentity,
    pub source: Source,
}

impl Selected {
    /// Bind the problem layer while preserving the caller's other invocation inputs.
    pub fn config_request(&self, request: ConfigRequest) -> ConfigRequest {
        ConfigRequest {
            workspace_root: Some(self.root.clone()),
            problem_file: Some(self.dir.join("problem.toml")),
            prospective_problem: None,
            ..request
        }
    }
}

#[derive(Debug)]
pub enum SelectionError {
    InvalidReference {
        reference: OsString,
        reason: String,
    },
    NotFound {
        identity: ProblemIdentity,
    },
    Collision {
        reason: CollisionReason,
        entries: Vec<PathBuf>,
        detail: String,
    },
    State {
        path: PathBuf,
        reason: String,
    },
    NoSelection,
    Storage(StorageError),
    Context {
        source: Source,
        description: String,
        cause: Box<SelectionError>,
    },
}

impl SelectionError {
    fn context(self, source: Source, description: String) -> Self {
        Self::Context {
            source,
            description,
            cause: Box::new(self),
        }
    }
}

impl fmt::Display for SelectionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidReference { reference, reason } => {
                write!(f, "invalid problem reference {reference:?}: {reason}")
            }
            Self::NotFound { identity } => write!(f, "problem {identity} not found"),
            Self::Collision {
                reason,
                entries,
                detail,
            } => {
                write!(f, "problem lookup {reason:?}: ")?;
                for (index, entry) in entries.iter().enumerate() {
                    if index > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{}", entry.display())?;
                }
                write!(f, "{detail}")
            }
            Self::State { path, reason } => {
                write!(f, "{} is {reason}", path.display())
            }
            Self::NoSelection => write!(
                f,
                "NoSelection: no problem selected; use cpf switch <ref> or --problem <ref>"
            ),
            Self::Storage(error) => write!(f, "{error}"),
            Self::Context {
                description, cause, ..
            } => write!(f, "{description}: {cause}"),
        }
    }
}

impl std::error::Error for SelectionError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Storage(error) => Some(error),
            Self::Context { cause, .. } => Some(&**cause),
            _ => None,
        }
    }
}

impl From<StorageError> for SelectionError {
    fn from(error: StorageError) -> Self {
        Self::Storage(error)
    }
}

impl From<SelectionError> for Error {
    fn from(error: SelectionError) -> Self {
        Self::Workspace(error.to_string())
    }
}

fn reference_identity(reference: &str) -> Result<ProblemIdentity, SelectionError> {
    let parsed = if reference.contains(':') {
        ProblemIdentity::parse(reference).map_err(|error| error.to_string())
    } else {
        Identifier::new(reference)
            .map(ProblemIdentity::local)
            .map_err(|error| error.to_string())
    };
    parsed.map_err(|reason| SelectionError::InvalidReference {
        reference: reference.into(),
        reason,
    })
}

/// Look up a reference without taking a lock. `root` is the canonical workspace
/// root. Mutating callers use this same lookup while holding their guard.
pub fn lookup(root: &Path, name: &str) -> Result<Problem, SelectionError> {
    let identity = reference_identity(name)?;
    let (resolution, detail) = inspect_problem(root, identity.name())?;
    match resolution {
        Resolution::Create => Err(SelectionError::NotFound { identity }),
        Resolution::Exists => Ok(Problem {
            dir: canonical(&root.join("problems").join(identity.name().as_str()))?,
            identity,
        }),
        Resolution::NameCollision { reason, entries } => Err(SelectionError::Collision {
            reason,
            entries: entries
                .into_iter()
                .map(|entry| root.join("problems").join(entry))
                .collect(),
            detail,
        }),
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct State {
    pub current_problem: Option<ProblemIdentity>,
}

fn state_fields(document: &DocumentMut) -> Result<State, String> {
    for (key, _) in document.iter() {
        if !matches!(key, "schema" | "current_problem") {
            return Err(format!("unknown key {key:?}"));
        }
    }
    let current_problem = document
        .get("current_problem")
        .map(|item| {
            let identity = item.as_str().ok_or("current_problem must be a string")?;
            ProblemIdentity::parse(identity).map_err(|error| error.to_string())
        })
        .transpose()?;
    Ok(State { current_problem })
}

/// Tool-only schema-first classification, including the shared 1 MiB bound.
pub fn classify_state(bytes: &[u8]) -> Record<State, String> {
    storage::classify_tool_record(bytes, 1, state_fields)
}

/// Read a state path; callers that mutate must first check it through their guard.
pub fn read_state(path: impl AsRef<Path>) -> Record<State, String> {
    storage::read_tool_record(path, 1, state_fields)
}

fn saved_state(root: &Path) -> Result<State, SelectionError> {
    let path =
        storage::checked_path(root, Path::new(".cpforge/state.toml"), true).map_err(|error| {
            SelectionError::from(error).context(Source::Saved, "saved selection".into())
        })?;
    match read_state(&path) {
        Record::Missing => Ok(State::default()),
        record => record_result(record).map_err(|reason| {
            SelectionError::State { path, reason }.context(Source::Saved, "saved selection".into())
        }),
    }
}

/// Publish the exact schema-1 representation through the storage primitive.
pub fn write_state(
    guard: &WorkspaceGuard<'_>,
    identity: &ProblemIdentity,
) -> Result<storage::Published, StorageError> {
    let bytes = format!("schema = 1\ncurrent_problem = \"{identity}\"\n");
    storage::atomic_replace(guard, ".cpforge/state.toml", bytes.as_bytes(), || Ok(()))
}

fn canonical(path: &Path) -> Result<PathBuf, SelectionError> {
    fs::canonicalize(path)
        .map(|path| storage::paths::plain(&path))
        .map_err(|error| StorageError::io(path, error).into())
}

fn path_component_eq(left: &OsStr, right: &OsStr) -> bool {
    // Storage §7 uses ASCII case folding on Windows and exact bytes on Unix.
    // Both complete paths have been canonicalised.
    if cfg!(windows) {
        left.as_encoded_bytes()
            .eq_ignore_ascii_case(right.as_encoded_bytes())
    } else {
        left == right
    }
}

fn cwd_name(root: &Path, physical_cwd: &Path) -> Option<OsString> {
    // A name Win32 would reinterpret keeps one side verbatim; compare both
    // without the drive or UNC verbatim prefix.
    let (root, physical_cwd) = (
        storage::paths::comparable(root),
        storage::paths::comparable(physical_cwd),
    );
    let mut components = physical_cwd.components();
    for expected in root.components() {
        let actual = components.next()?;
        if !path_component_eq(expected.as_os_str(), actual.as_os_str()) {
            return None;
        }
    }
    if !path_component_eq(components.next()?.as_os_str(), OsStr::new("problems")) {
        return None;
    }
    components
        .next()
        .map(|component| component.as_os_str().to_owned())
}

fn lookup_cwd(root: &Path, name: &OsStr) -> Result<Problem, SelectionError> {
    let result = match name.to_str() {
        Some(name) => Identifier::new(name)
            .map_err(|error| SelectionError::InvalidReference {
                reference: name.into(),
                reason: error.to_string(),
            })
            .and_then(|name| lookup(root, name.as_str())),
        None => Err(SelectionError::InvalidReference {
            reference: name.to_owned(),
            reason: "name is not valid Unicode".into(),
        }),
    };
    result.map_err(|error| {
        error.context(
            Source::Cwd,
            format!("this directory problems/{}", name.to_string_lossy()),
        )
    })
}

/// Resolve once using flag, physical cwd, then saved state. Only the winning
/// source's problem and (when needed) state record are inspected.
pub fn resolve(
    root: &Path,
    cwd: &Path,
    problem_flag: Option<&str>,
) -> Result<Selected, SelectionError> {
    crate::storage::crash::barrier("selection.resolve");
    let root = canonical(root)?;
    let (problem, source) = if let Some(reference) = problem_flag {
        (
            lookup(&root, reference).map_err(|error| {
                error.context(Source::Flag, format!("--problem flag {reference:?}"))
            })?,
            Source::Flag,
        )
    } else {
        let physical_cwd =
            canonical(cwd).map_err(|error| error.context(Source::Cwd, "this directory".into()))?;
        if let Some(name) = cwd_name(&root, &physical_cwd) {
            (lookup_cwd(&root, &name)?, Source::Cwd)
        } else {
            let state = saved_state(&root)?;
            let identity = state.current_problem.ok_or(SelectionError::NoSelection)?;
            (
                lookup(&root, &identity.to_string()).map_err(|error| {
                    error.context(Source::Saved, format!("saved identity {identity}"))
                })?,
                Source::Saved,
            )
        }
    };
    Ok(Selected {
        root,
        dir: problem.dir,
        identity: problem.identity,
        source,
    })
}

fn invocation_paths() -> Result<(PathBuf, PathBuf), Error> {
    let cwd = std::env::current_dir()
        .map_err(|error| Error::Workspace(format!("cannot read current directory: {error}")))?;
    let root = find_root(&cwd)?;
    Ok((root, cwd))
}

pub(crate) fn current(problem_flag: Option<&str>, path: bool) -> Result<(), Error> {
    let (root, cwd) = invocation_paths()?;
    let selected = resolve(&root, &cwd, problem_flag)?;
    let mut line = if path {
        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStrExt;
            selected.dir.as_os_str().as_bytes().to_vec()
        }
        #[cfg(not(unix))]
        {
            selected.dir.display().to_string().into_bytes()
        }
    } else {
        selected.identity.to_string().into_bytes()
    };
    line.push(b'\n');
    let mut output = io::stdout().lock();
    output
        .write_all(&line)
        .and_then(|()| output.flush())
        .map_err(|error| Error::Workspace(format!("cannot write selection: {error}")))
}

pub(crate) fn switch(
    reference: &str,
    config_path: Option<PathBuf>,
) -> Result<crate::exit::ExitCode, Error> {
    let (root, cwd) = invocation_paths()?;
    let lock = WorkspaceLock::new(root)?;
    let guard = lock.acquire("switch")?;
    if let Some(warning) = guard.recovery_warning() {
        for (path, error) in warning.paths.iter().zip(&warning.errors) {
            eprintln!(
                "cpf: warning: leftover recovery incomplete at {}: {error}",
                path.display()
            );
        }
    }
    let problem = lookup(guard.root(), reference)?;
    // Reject a link in a tool-written path, even on a same-selection no-op.
    guard.check_path(".cpforge/state.toml")?;
    let selected = Selected {
        root: guard.root().to_path_buf(),
        dir: problem.dir,
        identity: problem.identity,
        source: Source::Saved,
    };
    // Capture configuration and context during the metadata operation. A
    // configuration refusal belongs to the later hook job, not this commit.
    let config = crate::config::load_config(
        &selected.config_request(ConfigRequest {
            config_path,
            current_dir: cwd.clone(),
            environment: crate::config::Environment {
                home: std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" })
                    .map(PathBuf::from),
                xdg_config_home: std::env::var_os("XDG_CONFIG_HOME"),
                appdata: std::env::var_os("APPDATA"),
            },
            ..ConfigRequest::default()
        }),
    );
    let state = saved_state(guard.root())?;
    if state.current_problem.as_ref() != Some(&selected.identity) {
        let published = write_state(&guard, &selected.identity)?;
        let mut report = Report::default();
        publication_warnings(
            &mut report,
            &guard.root().join(".cpforge/state.toml"),
            &published,
        );
        for warning in report.warnings {
            eprintln!("cpf: warning: {warning}");
        }
    }
    drop(guard);
    match canonical(&cwd) {
        Ok(physical_cwd) => {
            if let Some(name) = cwd_name(&selected.root, &physical_cwd) {
                match lookup_cwd(&selected.root, &name) {
                    Ok(cwd_problem) if cwd_problem.identity != selected.identity => eprintln!(
                        "cpf: note: this directory still selects {}",
                        cwd_problem.identity
                    ),
                    Ok(_) => {}
                    Err(error) => {
                        eprintln!("cpf: note: cwd problem could not be resolved: {error}")
                    }
                }
            }
        }
        Err(error) => eprintln!("cpf: note: this directory could not be resolved: {error}"),
    }
    writeln!(io::stdout().lock(), "{}", selected.identity)
        .map_err(|error| Error::Workspace(format!("cannot write selection: {error}")))?;
    Ok(crate::job_command::switch_hooks(&selected, config))
}

/// Lock-free selection for `cpf status [problem|contest|--all]`. A reference
/// resolves through the single-problem [`lookup`] first; only an unqualified
/// name naming no problem falls back to a contest, and `contest:<name>`
/// selects a contest explicitly. Contest members keep `contest.toml` order;
/// `--all` lists the directories of `problems/` by name (plain files such as
/// editor or Finder metadata are not problems and are skipped).
pub(crate) fn select_many(
    root: &Path,
    cwd: &Path,
    reference: Option<&str>,
    all: bool,
    problem_flag: Option<&str>,
) -> Result<Vec<Problem>, Error> {
    if all {
        let directory = storage::checked_path(root, Path::new("problems"), false)?;
        let entries = match fs::read_dir(&directory) {
            Ok(entries) => entries,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(Error::Workspace(e.to_string())),
        };
        let mut names = Vec::new();
        for entry in entries {
            let entry = entry.map_err(|e| Error::Workspace(e.to_string()))?;
            if entry.file_type().is_ok_and(|kind| kind.is_file()) {
                continue;
            }
            let name = entry
                .file_name()
                .into_string()
                .map_err(|name| Error::Workspace(format!("invalid problem directory {name:?}")))?;
            names.push(name);
        }
        names.sort();
        return names
            .iter()
            .map(|name| lookup(root, name).map_err(Error::from))
            .collect();
    }
    let Some(reference) = reference else {
        let selected = resolve(root, cwd, problem_flag)?;
        return Ok(vec![Problem {
            dir: selected.dir,
            identity: selected.identity,
        }]);
    };
    let (contest, explicit) = if let Some(contest) = reference.strip_prefix("contest:") {
        (contest, true)
    } else {
        match lookup(root, reference) {
            Ok(problem) => return Ok(vec![problem]),
            Err(SelectionError::NotFound { .. }) if !reference.contains(':') => (reference, false),
            Err(error) => return Err(error.into()),
        }
    };
    let name = Identifier::new(contest)
        .map_err(|e| Error::Workspace(format!("invalid contest name {contest:?}: {e}")))?;
    let not_found = || {
        Error::Workspace(if explicit {
            format!("no contest named {name}")
        } else {
            format!("no problem or contest named {name}")
        })
    };
    let directory = storage::checked_path(root, Path::new("contests"), false)?;
    let entries = match fs::read_dir(&directory) {
        Ok(entries) => entries,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Err(not_found()),
        Err(e) => return Err(Error::Workspace(format!("{}: {e}", directory.display()))),
    };
    let mut matches = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|e| Error::Workspace(e.to_string()))?;
        if names::collides_with_native(name.as_str(), &entry.file_name()) {
            matches.push(entry.file_name());
        }
    }
    if matches.is_empty() {
        return Err(not_found());
    }
    if matches.as_slice() != [OsString::from(name.as_str())] {
        return Err(Error::Workspace(format!(
            "contest {name} is ambiguous or differently spelled: {matches:?}"
        )));
    }
    let path = storage::checked_path(
        root,
        &directory.join(name.as_str()).join("contest.toml"),
        false,
    )?;
    let record = storage::read_user_record(&path, 1, |document| {
        crate::add::contest_members(document)?;
        let mut ids = Vec::new();
        if let Some(records) = document.get("problem").and_then(Item::as_array_of_tables) {
            for record in records {
                ids.push(
                    record
                        .get("id")
                        .and_then(Item::as_str)
                        .expect("validated member")
                        .to_owned(),
                );
            }
        }
        Ok::<_, String>(ids)
    });
    let ids =
        record_result(record).map_err(|e| Error::Workspace(format!("{}: {e}", path.display())))?;
    ids.iter()
        .map(|id| lookup(root, id).map_err(Error::from))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::MalformedRecord;

    #[test]
    fn state_requires_schema_and_validates_only_supported_fields() {
        for bytes in [
            b"schema = 1\n".as_slice(),
            b"schema = 1\ncurrent_problem = 'local:A'\n",
        ] {
            assert!(matches!(classify_state(bytes), Record::Valid(_)));
        }
        assert!(matches!(
            classify_state(b"current_problem = 'local:A'"),
            Record::Malformed(MalformedRecord::SchemaMissing)
        ));
        assert!(matches!(
            classify_state(b"schema = '1'"),
            Record::Malformed(MalformedRecord::SchemaMalformed)
        ));
        assert!(matches!(
            classify_state(b"schema = 0"),
            Record::Malformed(MalformedRecord::SchemaTooOld { .. })
        ));
        // Unknown and incorrectly typed fields are not decoded at a newer schema.
        assert!(matches!(
            classify_state(b"schema = 2\ncurrent_problem = []\nunknown = true"),
            Record::TooNew {
                schema: 2,
                supported: 1
            }
        ));
        for text in [
            "schema = 1\nextra = true",
            "schema = 1\ncurrent_problem = []",
            "schema = 1\ncurrent_problem = 'A'",
            "schema = 1\ncurrent_problem = 'other:A'",
            "schema = 1\ncurrent_problem = 'local:a/b'",
        ] {
            assert!(
                matches!(
                    classify_state(text.as_bytes()),
                    Record::Malformed(MalformedRecord::Fields(_))
                ),
                "{text}"
            );
        }
        let oversized = vec![b' '; 1024 * 1024 + 1];
        assert!(matches!(
            classify_state(&oversized),
            Record::Malformed(MalformedRecord::TooLarge { .. })
        ));
    }

    #[test]
    fn cwd_lookup_preserves_all_existing_entry_refusals() {
        let mut bits = [0_u8; 16];
        getrandom::fill(&mut bits).unwrap();
        let root =
            std::env::temp_dir().join(format!("cpforge-cwd-{:032x}", u128::from_le_bytes(bits)));
        fs::create_dir_all(root.join("problems/Case")).unwrap();
        fs::write(
            root.join("problems/Case/problem.toml"),
            "[problem]\nid = 'local:Case'\n",
        )
        .unwrap();
        let root = canonical(&root).unwrap();
        // Physical cwd normally already has the directory entry's spelling and
        // cannot be a file or a link. Exercise those helper refusals directly.
        for (name, reason) in [
            ("case", CollisionReason::DifferentSpelling),
            ("file", CollisionReason::NotADirectory),
        ] {
            if name == "file" {
                fs::write(root.join("problems/file"), b"file").unwrap();
            }
            let error = lookup_cwd(&root, OsStr::new(name)).unwrap_err();
            assert!(
                matches!(error, SelectionError::Context { source: Source::Cwd, cause, .. }
                if matches!(*cause, SelectionError::Collision { reason: actual, .. } if actual == reason))
            );
        }
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink("Case", root.join("problems/link")).unwrap();
            let error = lookup_cwd(&root, OsStr::new("link")).unwrap_err();
            assert!(
                matches!(error, SelectionError::Context { source: Source::Cwd, cause, .. }
                if matches!(*cause, SelectionError::Collision { reason: CollisionReason::LinkInPath, .. }))
            );
        }
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn windows_canonical_ancestry_compares_components_with_ascii_case_folding() {
        let root = Path::new(r"C:\Work\ws");
        let cwd = Path::new(r"c:\work\WS\PROBLEMS\A\nested");
        assert_eq!(cwd_name(root, cwd), Some(OsString::from("A")));
        assert_eq!(
            cwd_name(root, Path::new(r"C:\Work\ws-extra\problems\A")),
            None
        );
        let root = Path::new(r"\\server\share\Work\ws");
        let cwd = Path::new(r"\\SERVER\SHARE\work\WS\PROBLEMS\A\nested");
        assert_eq!(cwd_name(root, cwd), Some(OsString::from("A")));
        // A nested name Win32 would reinterpret keeps the cwd verbatim.
        let cwd = storage::paths::plain(Path::new(r"\\?\UNC\server\share\Work\ws\problems\A\x."));
        assert_eq!(cwd_name(root, &cwd), Some(OsString::from("A")));
    }

    #[cfg(windows)]
    #[test]
    fn discovered_workspace_and_selected_cwd_use_plain_canonical_paths() {
        let mut bits = [0_u8; 16];
        getrandom::fill(&mut bits).unwrap();
        let root = std::env::temp_dir().join(format!(
            "cpforge-plain-root-{:032x}",
            u128::from_le_bytes(bits)
        ));
        fs::create_dir_all(root.join("problems/A/nested")).unwrap();
        fs::write(root.join("cpforge.toml"), "schema = 1\n").unwrap();
        fs::write(
            root.join("problems/A/problem.toml"),
            "[problem]\nid = 'local:A'\n",
        )
        .unwrap();
        let verbatim = fs::canonicalize(&root).unwrap();
        let discovered = find_root(&verbatim.join(r"problems\A\nested")).unwrap();
        assert!(
            !discovered
                .as_os_str()
                .as_encoded_bytes()
                .starts_with(br"\\?\")
        );
        assert_eq!(discovered, storage::paths::plain(&verbatim));
        let selected = resolve(&discovered, &verbatim.join(r"problems\A\nested"), None).unwrap();
        assert_eq!(selected.root, discovered);
        assert_eq!(selected.dir, discovered.join("problems/A"));
        assert_eq!(selected.source, Source::Cwd);
        let lock = WorkspaceLock::new(&verbatim).unwrap();
        let guard = lock.acquire("plain root").unwrap();
        assert_eq!(guard.root(), discovered.as_path());
        drop(guard);
        drop(lock);
        fs::remove_dir_all(root).unwrap();
    }
}
