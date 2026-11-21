//! User-editable test catalogs and read-only discovery.
//!
//! Mutations borrow the outer operation's guard. `add` publishes pre-lock
//! captures, then replays only its catalog insertion, never its file publication.
//! Freezing borrows a registered job's inputs capability, outside the workspace lock.

mod capture;
mod freeze;
pub use capture::CapturedFile;
pub use freeze::{FreezeError, FrozenAnswer, FrozenManifest, FrozenTest, TestId, TestSettings};

use std::collections::BTreeSet;
use std::fmt;
use std::fs::{self, OpenOptions};
use std::io;
use std::path::{Component, Path, PathBuf};

use toml_edit::{ArrayOfTables, DocumentMut, Item, Table, value};

use crate::names::{Identifier, PortableComponent, collision_key};
use crate::selection::record_result;
use crate::storage::{self, ErrorKind, Record, StorageError, WorkspaceGuard};
use crate::workspace::{Report, cleanup_warning, publication_warnings};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Location {
    pub file: PathBuf,
    pub key_path: String,
    pub line: Option<usize>,
    pub column: Option<usize>,
}

impl fmt::Display for Location {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.file.display())?;
        if let (Some(line), Some(column)) = (self.line, self.column) {
            write!(f, ":{line}:{column}")?;
        }
        if !self.key_path.is_empty() {
            write!(f, " ({})", self.key_path)?;
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Diagnostic {
    pub location: Location,
    pub message: String,
}

impl fmt::Display for Diagnostic {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.location, self.message)
    }
}

impl std::error::Error for Diagnostic {}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Origin {
    Sample,
    Custom,
    Stress,
    Imported,
}

impl Origin {
    fn as_str(self) -> &'static str {
        match self {
            Self::Sample => "sample",
            Self::Custom => "custom",
            Self::Stress => "stress",
            Self::Imported => "imported",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Answer {
    Absent,
    Named(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TestEntry {
    pub id: Identifier,
    pub input: String,
    pub answer: Answer,
    pub origin: Origin,
    pub enabled: bool,
    /// The catalog location of this entry, independent of its provenance.
    pub origin_location: Location,
}

impl TestEntry {
    fn paths(&self) -> impl Iterator<Item = &str> {
        std::iter::once(self.input.as_str()).chain(match &self.answer {
            Answer::Absent => None,
            Answer::Named(path) => Some(path.as_str()),
        })
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Catalog {
    pub entries: Vec<TestEntry>,
}

fn location(file: &Path, key: String, source: Option<&str>, offset: Option<usize>) -> Location {
    let position = source.zip(offset).map(|(source, offset)| {
        let prefix = &source[..offset];
        let line = prefix.bytes().filter(|byte| *byte == b'\n').count() + 1;
        let column = prefix.rsplit('\n').next().unwrap_or("").chars().count() + 1;
        (line, column)
    });
    Location {
        file: file.into(),
        key_path: key,
        line: position.map(|position| position.0),
        column: position.map(|position| position.1),
    }
}

fn validate_path(path: &str) -> Result<(), String> {
    let name = path
        .strip_prefix("tests/")
        .ok_or("path must be exactly tests/<portable component> using '/'")?;
    PortableComponent::new(name).map_err(|error| format!("invalid test path {path:?}: {error}"))?;
    Ok(())
}

fn fields(table: &Table, file: &Path, source: Option<&str>) -> Result<Catalog, Diagnostic> {
    let root_error = |message: &str| Diagnostic {
        location: location(
            file,
            "test".into(),
            source,
            table
                .get("test")
                .and_then(Item::span)
                .map(|span| span.start),
        ),
        message: message.into(),
    };
    let Some(item) = table.get("test") else {
        return Ok(Catalog::default());
    };
    let records = item
        .as_array_of_tables()
        .ok_or_else(|| root_error("test must be an array of records ([[test]])"))?;
    let mut ids = BTreeSet::new();
    let mut paths = BTreeSet::new();
    let mut entries = Vec::new();
    for (index, record) in records.iter().enumerate() {
        let at = |key: &str| {
            location(
                file,
                format!("test[{index}].{key}"),
                source,
                record
                    .get(key)
                    .and_then(Item::span)
                    .or_else(|| record.span())
                    .map(|span| span.start),
            )
        };
        let error = |key: &str, message: String| Diagnostic {
            location: at(key),
            message,
        };
        let string = |key: &str| {
            record
                .get(key)
                .and_then(Item::as_str)
                .ok_or_else(|| error(key, format!("{key} is required and must be a string")))
        };
        let id =
            Identifier::new(string("id")?).map_err(|reason| error("id", reason.to_string()))?;
        if !ids.insert(collision_key(id.as_str())) {
            return Err(error("id", format!("duplicate id by collision key: {id}")));
        }
        let input = string("input")?.to_owned();
        let answer = match record.get("answer") {
            None => Answer::Absent,
            Some(_) => Answer::Named(string("answer")?.to_owned()),
        };
        for (key, path) in std::iter::once(("input", input.as_str())).chain(match &answer {
            Answer::Absent => None,
            Answer::Named(path) => Some(("answer", path.as_str())),
        }) {
            validate_path(path).map_err(|reason| error(key, reason))?;
            if !paths.insert(collision_key(path)) {
                return Err(error(
                    key,
                    format!("duplicate path by collision key: {path}"),
                ));
            }
        }
        let origin = match string("origin")? {
            "sample" => Origin::Sample,
            "custom" => Origin::Custom,
            "stress" => Origin::Stress,
            "imported" => Origin::Imported,
            other => return Err(error("origin", format!("unknown origin {other:?}"))),
        };
        let enabled = record
            .get("enabled")
            .and_then(Item::as_bool)
            .ok_or_else(|| {
                error(
                    "enabled",
                    "enabled is required and must be a boolean".into(),
                )
            })?;
        entries.push(TestEntry {
            id,
            input,
            answer,
            origin,
            enabled,
            origin_location: at("id"),
        });
    }
    Ok(Catalog { entries })
}

/// Schema-first classification; unknown keys remain in the editable document.
pub fn classify_catalog(bytes: &[u8], file: &Path) -> Record<Catalog, Diagnostic> {
    storage::classify_user_record_spanned(bytes, 1, |document| {
        fields(document.as_table(), file, Some(document.raw()))
    })
}

pub fn read_catalog(file: &Path) -> Record<Catalog, Diagnostic> {
    match storage::read_record_bytes(file) {
        storage::RecordBytes::Missing => Record::Missing,
        storage::RecordBytes::Unreadable(error) => Record::Unreadable(error),
        storage::RecordBytes::TooLarge => Record::Malformed(storage::MalformedRecord::TooLarge {
            limit: storage::RECORD_SIZE_LIMIT,
        }),
        storage::RecordBytes::Present(bytes) => classify_catalog(&bytes, file),
    }
}

#[derive(Debug)]
pub enum Health {
    Ok { length: u64 },
    Missing,
    NotRegular { kind: &'static str },
    Unreadable(io::Error),
}

impl fmt::Display for Health {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Ok { .. } => write!(f, "ok"),
            Self::Missing => write!(f, "missing"),
            Self::NotRegular { kind } => write!(f, "not a regular file ({kind})"),
            Self::Unreadable(error) => write!(f, "unreadable ({error})"),
        }
    }
}

fn file_kind(kind: fs::FileType) -> &'static str {
    if kind.is_dir() {
        return "directory";
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::FileTypeExt;
        if kind.is_fifo() {
            return "FIFO";
        }
        if kind.is_socket() {
            return "socket";
        }
        if kind.is_char_device() {
            return "character device";
        }
        if kind.is_block_device() {
            return "block device";
        }
    }
    "other"
}

/// Open first, then inspect that handle. Links to regular data are allowed.
/// Nonblocking open prevents even a FIFO swapped in just before open from waiting.
pub(crate) fn open_regular(path: &Path) -> Result<fs::File, Health> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NONBLOCK | libc::O_CLOEXEC);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        use windows_sys::Win32::Storage::FileSystem::FILE_FLAG_BACKUP_SEMANTICS;
        options.custom_flags(FILE_FLAG_BACKUP_SEMANTICS);
    }
    match options
        .open(path)
        .and_then(|file| file.metadata().map(|metadata| (metadata, file)))
    {
        Ok((metadata, file)) if metadata.is_file() => Ok(file),
        Ok((metadata, _)) => Err(Health::NotRegular {
            kind: file_kind(metadata.file_type()),
        }),
        Err(error)
            if matches!(
                error.kind(),
                io::ErrorKind::NotFound | io::ErrorKind::NotADirectory
            ) =>
        {
            Err(Health::Missing)
        }
        Err(error) => {
            // Some types (notably sockets) cannot be opened as files. This
            // fallback only explains a refusal; healthy status always requires
            // metadata from the successfully opened handle above.
            Err(match fs::metadata(path) {
                Ok(metadata) if !metadata.is_file() => Health::NotRegular {
                    kind: file_kind(metadata.file_type()),
                },
                _ => Health::Unreadable(error),
            })
        }
    }
}

pub fn file_health(path: &Path) -> Health {
    match open_regular(path) {
        Ok(file) => match file.metadata() {
            Ok(metadata) => Health::Ok {
                length: metadata.len(),
            },
            Err(error) => Health::Unreadable(error),
        },
        Err(health) => health,
    }
}

#[derive(Debug)]
pub struct RegisteredEntry {
    pub entry: TestEntry,
    pub input: Health,
    pub answer: Option<Health>,
}

impl RegisteredEntry {
    pub fn answer_state(&self) -> &'static str {
        match &self.answer {
            None => "absent",
            Some(Health::Ok { length: 0 }) => "empty",
            // A named answer remains present in the catalog when unhealthy;
            // its health problem is reported separately. Empty requires a
            // healthy opened regular file with a known zero length.
            Some(_) => "present",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Candidate {
    pub input: String,
    pub answer: Answer,
}

#[derive(Debug)]
pub struct Listing {
    pub catalog: Record<Catalog, Diagnostic>,
    pub registered: Vec<RegisteredEntry>,
    pub candidates: Vec<Candidate>,
    pub strays: Vec<String>,
    pub diagnostics: Vec<Diagnostic>,
}

/// Catalog/store for one already-resolved problem directory.
#[derive(Clone, Debug)]
pub struct TestStore {
    directory: PathBuf,
}

#[derive(Clone, Debug)]
pub struct RegisterRequest {
    /// Lexical base for both paths; normally the invocation's physical cwd.
    pub current_dir: PathBuf,
    pub input: PathBuf,
    pub answer: Option<PathBuf>,
    pub no_answer: bool,
    pub id: Option<String>,
}

#[derive(Debug)]
pub struct Registration {
    pub id: Identifier,
    pub inferred_answer: Option<String>,
}

/// Warnings survive both successful publications and later failures.
#[derive(Debug)]
pub struct Mutation<T> {
    pub value: T,
    pub warnings: Vec<String>,
}

#[derive(Debug)]
pub struct StoreError {
    pub message: String,
    pub warnings: Vec<String>,
}

impl fmt::Display for StoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for StoreError {}

fn mutation<T>(
    operation: impl FnOnce(&mut Report) -> Result<T, String>,
) -> Result<Mutation<T>, StoreError> {
    let mut report = Report::default();
    match operation(&mut report) {
        Ok(value) => Ok(Mutation {
            value,
            warnings: report.warnings,
        }),
        Err(message) => Err(StoreError {
            message,
            warnings: report.warnings,
        }),
    }
}

/// Steps after allocation; failures never roll back a published user file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AddStep {
    InputPublication,
    AnswerPublication,
    Registration,
}

impl fmt::Display for AddStep {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::InputPublication => "step 2 (input publication)",
            Self::AnswerPublication => "step 2 (answer publication)",
            Self::Registration => "step 3 (registration)",
        })
    }
}

#[derive(Debug)]
pub struct AddFailure {
    pub step: AddStep,
    pub cause: String,
}

/// Once a number is allocated, even a failure returns this outcome. `published`
/// lists only committed data paths, relative to the problem. A schema-only
/// catalog creation does not count as registration. Earlier refusal/allocation
/// errors return `StoreError` and have published no data.
#[derive(Debug)]
pub struct AddOutcome {
    pub id: Identifier,
    pub published: Vec<PathBuf>,
    pub registered: bool,
    pub failure: Option<AddFailure>,
    pub warnings: Vec<String>,
}

impl TestStore {
    /// `problem_dir` is the canonical absolute directory from the shared resolver.
    pub fn new(problem_dir: PathBuf) -> Self {
        Self {
            directory: problem_dir,
        }
    }

    fn catalog_path(&self) -> PathBuf {
        self.directory.join("tests.toml")
    }

    /// Read registered entries only, without discovery or opening test data.
    pub fn catalog(&self) -> Result<Catalog, String> {
        let path = self.catalog_path();
        match read_catalog(&path) {
            Record::Missing => Ok(Catalog::default()),
            record => {
                record_result(record).map_err(|reason| format!("{} is {reason}", path.display()))
            }
        }
    }

    /// Lock-free: no recovery, lock creation, or writes. Invalid metadata does
    /// not prevent discovery. Only failure to enumerate tests/ fails the listing.
    pub fn list(&self) -> Result<Listing, StorageError> {
        let catalog = read_catalog(&self.catalog_path());
        let mut listing = Listing {
            catalog,
            registered: Vec::new(),
            candidates: Vec::new(),
            strays: Vec::new(),
            diagnostics: Vec::new(),
        };
        let reason = match &listing.catalog {
            Record::Missing | Record::Valid(_) => None,
            Record::Unreadable(error) => Some(format!("unreadable: {error}")),
            Record::TooNew { schema, supported } => {
                Some(format!("too_new: schema {schema}, supported {supported}"))
            }
            Record::Malformed(storage::MalformedRecord::Fields(error)) => {
                let mut diagnostic = error.clone();
                diagnostic.message = format!("malformed: {}", diagnostic.message);
                listing.diagnostics.push(diagnostic);
                None
            }
            Record::Malformed(storage::MalformedRecord::Toml(error)) => {
                Some(format!("malformed: {error}"))
            }
            Record::Malformed(error) => Some(format!("malformed: {error}")),
        };
        if let Some(message) = reason {
            listing.diagnostics.push(Diagnostic {
                location: location(&self.catalog_path(), String::new(), None, None),
                message,
            });
        }
        let mut named = BTreeSet::new();
        if let Record::Valid(catalog) = &listing.catalog {
            for entry in &catalog.entries {
                named.extend(entry.paths().map(collision_key));
                let input = file_health(&self.directory.join(&entry.input));
                let answer = match &entry.answer {
                    Answer::Absent => None,
                    Answer::Named(path) => Some(file_health(&self.directory.join(path))),
                };
                listing.registered.push(RegisteredEntry {
                    entry: entry.clone(),
                    input,
                    answer,
                });
            }
        }
        let directory = self.directory.join("tests");
        let entries = match fs::read_dir(&directory) {
            Ok(entries) => entries,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(listing),
            Err(error) => return Err(StorageError::io(directory, error)),
        };
        let mut names = Vec::new();
        for entry in entries {
            let entry = entry.map_err(|error| StorageError::io(&directory, error))?;
            let name = entry.file_name();
            let Some(name) = name.to_str() else {
                listing.diagnostics.push(Diagnostic {
                    location: location(&entry.path(), String::new(), None, None),
                    message: "test filename is not valid Unicode".into(),
                });
                continue;
            };
            if name.to_ascii_lowercase().starts_with(".cpf-tmp-") {
                continue;
            }
            if let Err(error) = PortableComponent::new(name) {
                listing.diagnostics.push(Diagnostic {
                    location: location(&entry.path(), String::new(), None, None),
                    message: format!("invalid test filename: {error}"),
                });
                continue;
            }
            names.push(name.to_owned());
        }
        names.sort_by_cached_key(|name| (collision_key(name), name.clone()));
        let present: BTreeSet<_> = names.iter().cloned().collect();
        let mut paired = BTreeSet::new();
        for name in &names {
            if let Some(stem) = name.strip_suffix(".in") {
                let input = format!("tests/{name}");
                if named.contains(&collision_key(&input)) {
                    continue;
                }
                let sibling = format!("{stem}.ans");
                // A sibling already registered in either role is not offered.
                let answer = if present.contains(&sibling)
                    && !named.contains(&collision_key(&format!("tests/{sibling}")))
                {
                    paired.insert(sibling.clone());
                    Answer::Named(format!("tests/{sibling}"))
                } else {
                    Answer::Absent
                };
                listing.candidates.push(Candidate { input, answer });
            }
        }
        for name in names {
            let path = format!("tests/{name}");
            if name.ends_with(".ans")
                && !named.contains(&collision_key(&path))
                && !paired.contains(&name)
            {
                listing.strays.push(path);
            }
        }
        listing
            .diagnostics
            .sort_by_key(|diagnostic| diagnostic.location.file.clone());
        Ok(listing)
    }

    fn checked_catalog(&self, guard: &WorkspaceGuard<'_>) -> Result<PathBuf, String> {
        guard
            .check_path(self.catalog_path())
            .map_err(|error| error.to_string())
    }

    fn mutable_catalog(&self, guard: &WorkspaceGuard<'_>) -> Result<Catalog, String> {
        let path = self.checked_catalog(guard)?;
        match read_catalog(&path) {
            Record::Missing => Ok(Catalog::default()),
            record => {
                record_result(record).map_err(|reason| format!("{} is {reason}", path.display()))
            }
        }
    }

    /// Shared no-clobber bootstrap for register and add.
    fn ensure_catalog(
        &self,
        guard: &WorkspaceGuard<'_>,
        report: &mut Report,
    ) -> Result<PathBuf, String> {
        let path = self.checked_catalog(guard)?;
        if matches!(read_catalog(&path), Record::Missing) {
            match storage::publish_file_noclobber(guard, &path, b"schema = 1\n") {
                Ok(published) => publication_warnings(report, &path, &published),
                Err(error) if matches!(error.kind, ErrorKind::Exists { .. }) => {
                    cleanup_warning(report, &error.cleanup)
                }
                Err(error) => {
                    cleanup_warning(report, &error.cleanup);
                    return Err(error.to_string());
                }
            }
        }
        // A no-clobber collision is classified afresh, never overwritten.
        record_result(read_catalog(&path))
            .map_err(|reason| format!("{} is {reason}", path.display()))?;
        Ok(path)
    }

    /// Publish captures made before acquiring `guard`. Recovery belongs to the
    /// outer lock acquisition, never to this nested operation.
    pub fn add(
        &self,
        guard: &WorkspaceGuard<'_>,
        input: &CapturedFile,
        answer: Option<&CapturedFile>,
        origin: Origin,
    ) -> Result<AddOutcome, StoreError> {
        storage::crash::point("test_add.locked");
        let allocated = mutation(|_| {
            let catalog = self.mutable_catalog(guard)?;
            self.allocate(guard, &catalog, answer.is_some(), origin)
        })
        .map_err(|mut error| {
            error.message = format!("step 1 (catalog and allocation) failed: {}", error.message);
            error
        })?;
        let entry = allocated.value;
        let mut outcome = AddOutcome {
            id: entry.id.clone(),
            published: Vec::new(),
            registered: false,
            failure: None,
            warnings: Vec::new(),
        };
        let mut report = Report::default();
        let mut step = AddStep::InputPublication;
        let result = (|| {
            guard
                .create_dir_all(self.directory.join("tests"))
                .map_err(|error| error.to_string())?;
            for (path, capture, phase) in
                std::iter::once((entry.input.as_str(), input, "test_add.input_published")).chain(
                    match (&entry.answer, answer) {
                        (Answer::Named(path), Some(capture)) => {
                            Some((path.as_str(), capture, "test_add.answer_published"))
                        }
                        _ => None,
                    },
                )
            {
                if phase == "test_add.answer_published" {
                    step = AddStep::AnswerPublication;
                }
                let mut reader = capture.reader().map_err(|error| error.to_string())?;
                let published = storage::publish_file_noclobber_from_reader(
                    guard,
                    self.directory.join(path),
                    &mut reader,
                )
                .map_err(|error| {
                    cleanup_warning(&mut report, &error.cleanup);
                    error.to_string()
                })?;
                outcome.published.push(path.into());
                publication_warnings(&mut report, &self.directory.join(path), &published);
                storage::crash::point(phase);
            }
            step = AddStep::Registration;
            let path = self.ensure_catalog(guard, &mut report)?;
            let published = storage::edit_toml_replay(
                guard,
                &path,
                |document| check_catalog(document, &path),
                |document| insert_entry(document, &path, &entry),
            )
            .map_err(|error| {
                edit_cleanup_warnings(&mut report, &error);
                format!("{}: {error}", path.display())
            })?;
            outcome.registered = published.committed;
            publication_warnings(&mut report, &path, &published);
            storage::crash::point("test_add.registered");
            Ok::<(), String>(())
        })();
        if let Err(cause) = result {
            outcome.failure = Some(AddFailure { step, cause });
        }
        outcome.warnings = report.warnings;
        Ok(outcome)
    }

    fn allocate(
        &self,
        guard: &WorkspaceGuard<'_>,
        catalog: &Catalog,
        with_answer: bool,
        origin: Origin,
    ) -> Result<TestEntry, String> {
        let directory = guard
            .check_path(self.directory.join("tests"))
            .map_err(|error| error.to_string())?;
        let mut occupied = BTreeSet::new();
        match fs::read_dir(&directory) {
            Ok(entries) => {
                for entry in entries {
                    let entry = entry.map_err(|error| error.to_string())?;
                    if let Some(name) = entry.file_name().to_str() {
                        occupied.insert(collision_key(name));
                    }
                    // Generated ASCII names have no non-Unicode native spelling.
                }
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(format!("{}: {error}", directory.display())),
        }
        let ids: BTreeSet<_> = catalog
            .entries
            .iter()
            .map(|entry| collision_key(entry.id.as_str()))
            .collect();
        let paths: BTreeSet<_> = catalog
            .entries
            .iter()
            .flat_map(TestEntry::paths)
            .map(collision_key)
            .collect();
        for n in 1..=u64::MAX {
            let id = format!("custom-{n}");
            let input = format!("tests/{id}.in");
            let answer = format!("tests/{id}.ans");
            if ids.contains(&collision_key(&id))
                || [&input, &answer].iter().any(|path| {
                    paths.contains(&collision_key(path))
                        || occupied.contains(&collision_key(
                            path.strip_prefix("tests/").expect("generated path"),
                        ))
                })
            {
                continue;
            }
            return Ok(TestEntry {
                id: Identifier::new(id).expect("generated identifier"),
                input,
                answer: if with_answer {
                    Answer::Named(answer)
                } else {
                    Answer::Absent
                },
                origin,
                enabled: true,
                origin_location: location(&self.catalog_path(), String::new(), None, None),
            });
        }
        Err("custom test number space exhausted".into())
    }

    pub fn register(
        &self,
        guard: &WorkspaceGuard<'_>,
        request: &RegisterRequest,
    ) -> Result<Mutation<Registration>, StoreError> {
        mutation(|report| {
            let catalog = self.mutable_catalog(guard)?;
            if request.no_answer && request.answer.is_some() {
                return Err("--answer and --no-answer conflict".into());
            }
            let tests = self.directory.join("tests");
            let input = self.registration_path(&request.current_dir, &request.input)?;
            let mut inferred_answer = None;
            let answer = if let Some(answer) = &request.answer {
                Answer::Named(self.registration_path(&request.current_dir, answer)?)
            } else if request.no_answer {
                Answer::Absent
            } else {
                // Same exact-spelling rule as discovery, so a case-insensitive
                // filesystem cannot pair `x.in` with `X.ANS` here but not in ls.
                let sibling = Path::new(&input).with_extension("ans");
                let name = sibling
                    .file_name()
                    .and_then(|name| name.to_str())
                    .ok_or("answer filename is not Unicode")?;
                let path = format!("tests/{name}");
                if path != input && exact_entry(&tests, name)? {
                    validate_path(&path)?;
                    inferred_answer = Some(path.clone());
                    Answer::Named(path)
                } else {
                    Answer::Absent
                }
            };
            let id = if let Some(id) = &request.id {
                Identifier::new(id.clone()).map_err(|error| format!("invalid --id: {error}"))?
            } else {
                let stem = Path::new(&input)
                    .file_stem()
                    .and_then(|stem| stem.to_str())
                    .ok_or("input has no stem; supply --id")?;
                let id = Identifier::new(stem).map_err(|error| {
                    format!("input stem is not a valid id ({error}); supply --id")
                })?;
                if catalog
                    .entries
                    .iter()
                    .any(|entry| collision_key(entry.id.as_str()) == collision_key(id.as_str()))
                {
                    return Err(format!("input stem id {id} is taken; supply --id"));
                }
                id
            };
            let entry = TestEntry {
                id: id.clone(),
                input,
                answer,
                origin: Origin::Custom,
                enabled: true,
                origin_location: location(&self.catalog_path(), String::new(), None, None),
            };
            for path in entry.paths() {
                let health = file_health(&self.directory.join(path));
                if !matches!(health, Health::Ok { .. }) {
                    return Err(format!("{path}: {health}"));
                }
                // A case-insensitive filesystem opens `tests/X.in` for `x.in`;
                // the catalog must still record the name as spelled on disk.
                let name = path.strip_prefix("tests/").expect("validated test path");
                if !exact_entry(&tests, name)? {
                    return Err(format!(
                        "{path}: no entry of tests/ is spelled exactly like this; use the name as it appears on disk"
                    ));
                }
            }
            available(&catalog, &entry)?;
            let path = self.ensure_catalog(guard, report)?;
            let published = storage::edit_toml_replay(
                guard,
                &path,
                |document| check_catalog(document, &path),
                |document| insert_entry(document, &path, &entry),
            )
            .map_err(|error| format!("{}: {error}", path.display()))?;
            publication_warnings(report, &path, &published);
            Ok(Registration {
                id,
                inferred_answer,
            })
        })
    }

    /// Locate by exact stable id on each replay; never remember a table index.
    /// The returned bool says whether the catalog was rewritten.
    pub fn set_enabled(
        &self,
        guard: &WorkspaceGuard<'_>,
        id: &str,
        enabled: bool,
    ) -> Result<Mutation<bool>, StoreError> {
        mutation(|report| {
            self.mutable_catalog(guard)?;
            Identifier::new(id).map_err(|error| error.to_string())?;
            let path = self.checked_catalog(guard)?;
            if matches!(read_catalog(&path), Record::Missing) {
                return Err(unknown_id(id));
            }
            let published = storage::edit_toml_replay(
                guard,
                &path,
                |document| check_catalog(document, &path),
                |document| {
                    let record = document
                        .get_mut("test")
                        .and_then(Item::as_array_of_tables_mut)
                        .and_then(|records| {
                            records
                                .iter_mut()
                                .find(|record| record.get("id").and_then(Item::as_str) == Some(id))
                        })
                        .ok_or_else(|| unknown_id(id))?;
                    if record.get("enabled").and_then(Item::as_bool) != Some(enabled) {
                        // Replace the value only, preserving its key and comment decor.
                        let item = record.get_mut("enabled").expect("validated enabled");
                        let decor = item.as_value().expect("boolean value").decor().clone();
                        *item = value(enabled);
                        *item.as_value_mut().expect("boolean value").decor_mut() = decor;
                    }
                    Ok(())
                },
            )
            .map_err(|error| format!("{}: {error}", path.display()))?;
            publication_warnings(report, &path, &published);
            Ok(published.committed)
        })
    }

    fn registration_path(&self, cwd: &Path, path: &Path) -> Result<String, String> {
        let absolute = lexical_absolute(cwd, path)?;
        let directory = lexical_absolute(&self.directory, &self.directory.join("tests"))?;
        let name = absolute
            .strip_prefix(&directory)
            .ok()
            .filter(|relative| relative.components().count() == 1)
            .and_then(Path::to_str)
            .ok_or_else(|| {
                format!(
                    "{} must resolve lexically to tests/<name> of the selected problem \
                     (paths are relative to the current directory)",
                    path.display()
                )
            })?;
        let path = format!("tests/{name}");
        validate_path(&path)?;
        Ok(path)
    }
}

fn edit_cleanup_warnings(report: &mut Report, error: &storage::EditError<String>) {
    match error {
        storage::EditError::Storage(error) => cleanup_warning(report, &error.cleanup),
        storage::EditError::Conflict {
            cleanup,
            previous_cleanup,
            ..
        } => {
            cleanup_warning(report, cleanup);
            cleanup_warning(report, previous_cleanup);
        }
        storage::EditError::Replay { error, cleanup } => {
            cleanup_warning(report, cleanup);
            edit_cleanup_warnings(report, error);
        }
        _ => {}
    }
}

fn lexical_absolute(cwd: &Path, path: &Path) -> Result<PathBuf, String> {
    if !cwd.is_absolute() {
        return Err("current directory must be absolute".into());
    }
    let joined = cwd.join(path);
    let mut result = PathBuf::new();
    for component in joined.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                result.pop();
            }
            component => result.push(component.as_os_str()),
        }
    }
    // CLI absolute paths may use either the plain or the verbatim drive/UNC
    // form; compare lexical components in the same form, preserving even
    // non-Unicode ancestor names.
    Ok(crate::storage::paths::comparable(&result))
}

/// Whether `directory` has an entry spelled exactly `name` (any type).
fn exact_entry(directory: &Path, name: &str) -> Result<bool, String> {
    let entries = match fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(format!("{}: {error}", directory.display())),
    };
    for entry in entries {
        let entry = entry.map_err(|error| format!("{}: {error}", directory.display()))?;
        if entry.file_name().as_os_str() == std::ffi::OsStr::new(name) {
            return Ok(true);
        }
    }
    Ok(false)
}

fn unknown_id(id: &str) -> String {
    format!(
        "unknown registered test id {id:?}; an unregistered candidate must be registered first with cpf test register"
    )
}

fn check_catalog(document: &DocumentMut, path: &Path) -> Result<(), String> {
    if let Some(schema) = document.get("schema") {
        let schema = schema
            .as_integer()
            .ok_or("malformed: schema must be an integer")?;
        if schema > 1 {
            return Err(format!("too_new: schema {schema}, supported 1"));
        }
        if schema < 1 {
            return Err(format!(
                "malformed: schema {schema} is older than supported 1"
            ));
        }
    }
    fields(document.as_table(), path, None)
        .map(|_| ())
        .map_err(|error| format!("malformed: {error}"))
}

fn available(catalog: &Catalog, entry: &TestEntry) -> Result<(), String> {
    let mut paths = BTreeSet::new();
    for path in entry.paths() {
        if !paths.insert(collision_key(path)) {
            return Err(format!("conflict: path {path} is used in both roles"));
        }
    }
    for existing in &catalog.entries {
        if collision_key(existing.id.as_str()) == collision_key(entry.id.as_str()) {
            return Err(format!("conflict: test id {} is taken", entry.id));
        }
        for path in existing.paths() {
            if paths.contains(&collision_key(path)) {
                return Err(format!(
                    "conflict: path {path} is already named by an entry"
                ));
            }
        }
    }
    Ok(())
}

/// Semantic insertion shared by add and register: checked against this
/// attempt's fresh document, including paths reserved by missing files.
fn insert_entry(document: &mut DocumentMut, path: &Path, entry: &TestEntry) -> Result<(), String> {
    let catalog = fields(document.as_table(), path, None).map_err(|error| error.to_string())?;
    available(&catalog, entry)?;
    let mut record = Table::new();
    record.decor_mut().set_prefix("\n");
    record["id"] = value(entry.id.as_str());
    record["input"] = value(entry.input.as_str());
    if let Answer::Named(answer) = &entry.answer {
        record["answer"] = value(answer.as_str());
    }
    record["origin"] = value(entry.origin.as_str());
    record["enabled"] = value(entry.enabled);
    if document.get("test").is_none() {
        document["test"] = Item::ArrayOfTables(ArrayOfTables::new());
    }
    document["test"]
        .as_array_of_tables_mut()
        .expect("validated test array")
        .push(record);
    Ok(())
}

#[cfg(test)]
mod tests;
