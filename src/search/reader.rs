//! Charged workspace traversal. No workspace lock, recovery, or per-candidate
//! directory lookup: type and collision decisions use the single inventory.
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs::{self, OpenOptions};
use std::io::{self, Read};
use std::path::Path;
use std::sync::Mutex;

use super::{Results, match_problem, source};
use crate::cli::SearchArgs;
use crate::error::Error;
use crate::jobs::engine::CancelState;
use crate::names::{self, Identifier, ProblemIdentity};
use crate::organisation;
use crate::organisation_read::{self as read, Contest, Problem, Source};
use crate::storage;

const NOTES_LIMIT: usize = 65_536;

struct Budget<'a> {
    cancel: &'a CancelState,
    shared: &'a Mutex<Results>,
    entries: u32,
    bytes: u64,
    max_entries: u32,
    max_bytes: u64,
    #[cfg(feature = "storage-test-hooks")]
    reads: u32,
    #[cfg(feature = "storage-test-hooks")]
    barrier_action: std::cell::Cell<u32>,
}
#[derive(Debug)]
struct Stop;

impl<'a> Budget<'a> {
    fn new(cancel: &'a CancelState, shared: &'a Mutex<Results>) -> Self {
        let limits = (100_000, 67_108_864);
        #[cfg(feature = "storage-test-hooks")]
        let limits = (
            std::env::var("CPF_STORAGE_SEARCH_ENTRIES")
                .ok()
                .and_then(|v| v.parse::<u32>().ok())
                .unwrap_or(limits.0)
                .min(limits.0),
            std::env::var("CPF_STORAGE_SEARCH_BYTES")
                .ok()
                .and_then(|v| v.parse::<u64>().ok())
                .unwrap_or(limits.1)
                .min(limits.1),
        );
        Self {
            cancel,
            shared,
            entries: 0,
            bytes: 0,
            max_entries: limits.0,
            max_bytes: limits.1,
            #[cfg(feature = "storage-test-hooks")]
            reads: 0,
            #[cfg(feature = "storage-test-hooks")]
            barrier_action: std::cell::Cell::new(0),
        }
    }
    fn stopped(&self) -> Result<(), Stop> {
        if self.cancel.cancellation().is_some() {
            Err(Stop)
        } else {
            Ok(())
        }
    }
    fn reason(&self, reason: usize) -> Stop {
        self.shared.lock().unwrap().reasons[reason] = true;
        Stop
    }
    fn before(&self) -> Result<(), Stop> {
        self.stopped()?;
        if self.entries >= self.max_entries {
            return Err(self.reason(0));
        }
        #[cfg(feature = "storage-test-hooks")]
        if self.barrier_action.replace(self.entries + 1) != self.entries + 1 {
            storage::crash::barrier(&format!("search.before_action#{}", self.entries + 1));
        }
        self.stopped()
    }
    fn charge(&mut self) {
        self.entries += 1;
        self.shared.lock().unwrap().entries = self.entries;
    }
    fn trace(&self, call: &str, path: &Path, bytes: usize) {
        #[cfg(feature = "storage-test-hooks")]
        if let Some(path_log) = std::env::var_os("CPF_STORAGE_SEARCH_IO") {
            use std::io::Write;
            let mut log = OpenOptions::new()
                .create(true)
                .append(true)
                .open(path_log)
                .expect("open search recorder");
            let action = self.entries + u32::from(call == "enumerate");
            writeln!(log, "{action}\t{call}\t{}\t{bytes}", path.display())
                .expect("record search call");
        }
        #[cfg(not(feature = "storage-test-hooks"))]
        let _ = (call, path, bytes);
    }
    fn inventory(
        &mut self,
        root: &Path,
        directory: &str,
    ) -> Result<Result<Inventory, Error>, Stop> {
        self.before()?;
        self.trace("enumerate", Path::new(directory), 0);
        // The containment check, directory handle and iterator belong to the
        // enumeration operation; each returned entry (including its type) is
        // charged separately. A linked `problems/` is refused as `cpf ls` does.
        let dir = match storage::checked_path(root, Path::new(directory), false) {
            Ok(dir) => dir,
            Err(error) => return Ok(Err(error.into())),
        };
        let mut listing = match fs::read_dir(dir) {
            Ok(listing) => listing,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Ok(Ok(Inventory::default()));
            }
            Err(error) => return Ok(Err(Error::Workspace(format!("{directory}: {error}")))),
        };
        let mut inventory = Inventory::default();
        loop {
            self.before()?;
            let entry = match listing.next() {
                None => {
                    self.trace("enumeration_end", Path::new(directory), 0);
                    break;
                }
                Some(Ok(entry)) => entry,
                Some(Err(error)) => {
                    return Ok(Err(Error::Workspace(format!("{directory}: {error}"))));
                }
            };
            self.charge();
            let path = Path::new(directory).join(entry.file_name());
            self.trace("entry", &path, 0);
            let kind = entry
                .file_type()
                .map_err(|error| source(&path, "metadata_unreadable", error));
            self.trace("entry_type", &path, 0);
            if let Some(name) = entry.file_name().to_str() {
                *inventory
                    .keys
                    .entry(names::collision_key(name))
                    .or_default() += 1;
            }
            inventory.entries.push(Entry {
                name: entry.file_name(),
                kind,
            });
            // Hitting E during enumeration must omit every candidate, even
            // when the final entry happened to exhaust the allowance exactly.
            if self.entries == self.max_entries {
                return Err(self.reason(0));
            }
        }
        inventory
            .entries
            .sort_by(|a, b| a.name.as_encoded_bytes().cmp(b.name.as_encoded_bytes()));
        Ok(Ok(inventory))
    }
    fn file(&mut self, root: &Path, path: &Path, notes: bool) -> Result<FileBytes, Stop> {
        self.before()?;
        self.charge();
        self.trace("open", path, 0);
        let invalid =
            |kind, error: &dyn std::fmt::Display| FileBytes::Invalid(source(path, kind, error));
        let mut options = OpenOptions::new();
        options.read(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC);
        }
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt;
            use windows_sys::Win32::Storage::FileSystem::{
                FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT,
            };
            options.custom_flags(FILE_FLAG_OPEN_REPARSE_POINT | FILE_FLAG_BACKUP_SEMANTICS);
        }
        let mut file = match options.open(root.join(path)) {
            Ok(file) => file,
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::NotFound | io::ErrorKind::NotADirectory
                ) =>
            {
                return Ok(FileBytes::Missing);
            }
            #[cfg(unix)]
            Err(error) if notes && error.raw_os_error() == Some(libc::ELOOP) => {
                return Ok(invalid("notes_not_regular", &error));
            }
            Err(error) => {
                return Ok(invalid(
                    if notes {
                        "notes_unreadable"
                    } else {
                        "metadata_unreadable"
                    },
                    &error,
                ));
            }
        };
        self.trace("handle_metadata", path, 0);
        let metadata = match file.metadata() {
            Ok(metadata) => metadata,
            Err(error) => {
                return Ok(invalid(
                    if notes {
                        "notes_unreadable"
                    } else {
                        "metadata_unreadable"
                    },
                    &error,
                ));
            }
        };
        if !metadata.is_file() || storage::is_link(&metadata) {
            return Ok(invalid(
                if notes {
                    "notes_not_regular"
                } else {
                    "metadata_unreadable"
                },
                &"not a regular file",
            ));
        }
        // An oversized record is classified from its handle length, unread,
        // exactly as the shared record reader does.
        if !notes && metadata.len() > storage::RECORD_SIZE_LIMIT as u64 {
            return Ok(FileBytes::TooLarge);
        }
        let cap = if notes {
            NOTES_LIMIT + 1
        } else {
            storage::RECORD_SIZE_LIMIT + 1
        };
        let expected = metadata.len().min(cap as u64);
        let required = if notes { expected } else { metadata.len() };
        if required > self.max_bytes - self.bytes {
            return Err(self.reason(1));
        }
        let mut bytes = Vec::new();
        // Read once from this handle, in chunks; never retry/reopen a torn file.
        // The first chunk ends halfway through the observed length for SG-73.
        let mut chunk = vec![0; (expected as usize / 2).max(1)];
        while bytes.len() < cap {
            self.stopped()?;
            let remaining = self.max_bytes - self.bytes;
            let length = (cap - bytes.len()).min(chunk.len()).min(remaining as usize);
            if length == 0 {
                // The observed length is exhausted: no read beyond B is
                // permitted. A handle length check detects growth at no cost E.
                self.trace("handle_length", path, 0);
                if !file.metadata().is_ok_and(|m| m.len() <= bytes.len() as u64) {
                    return Err(self.reason(1));
                }
                break;
            }
            #[cfg(feature = "storage-test-hooks")]
            {
                self.reads += 1;
                storage::crash::barrier(&format!("search.read#{}", self.reads));
            }
            let read = file.read(&mut chunk[..length]);
            self.trace("read", path, read.as_ref().copied().unwrap_or(0));
            let count = match read {
                Ok(count) => count,
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(error) => {
                    return Ok(invalid(
                        if notes {
                            "notes_unreadable"
                        } else {
                            "metadata_unreadable"
                        },
                        &error,
                    ));
                }
            };
            self.bytes += count as u64;
            self.shared.lock().unwrap().bytes_read = self.bytes;
            if count == 0 {
                break;
            }
            bytes.extend_from_slice(&chunk[..count]);
            #[cfg(feature = "storage-test-hooks")]
            if bytes.len() == (expected as usize / 2).max(1) {
                storage::crash::barrier("org.read_partial");
            }
        }
        if !notes && bytes.len() > storage::RECORD_SIZE_LIMIT {
            return Ok(FileBytes::TooLarge);
        }
        Ok(FileBytes::Present(bytes))
    }
}

#[derive(Default)]
struct Inventory {
    entries: Vec<Entry>,
    keys: BTreeMap<String, usize>,
}
struct Entry {
    name: OsString,
    kind: Result<fs::FileType, Source>,
}
impl Inventory {
    fn validate(&self, entry: &Entry, directory: &str) -> Result<Identifier, Source> {
        let path = Path::new(directory).join(&entry.name);
        let id = Identifier::from_os_str(&entry.name)
            .map_err(|error| source(&path, "invalid_name", error))?;
        if self.keys.get(&names::collision_key(id.as_str())) != Some(&1) {
            return Err(source(&path, "name_collision", "colliding directory names"));
        }
        let kind = entry.kind.as_ref().map_err(Clone::clone)?;
        if kind.is_symlink() {
            return Err(source(&path, "link", "entry is a link"));
        }
        if !kind.is_dir() {
            return Err(source(&path, "not_a_directory", "entry is not a directory"));
        }
        Ok(id)
    }
}
enum FileBytes {
    Missing,
    Invalid(Source),
    TooLarge,
    Present(Vec<u8>),
}
impl FileBytes {
    fn document(self, path: &Path, contest: bool) -> Result<toml_edit::DocumentMut, Source> {
        let record = match self {
            Self::Missing => storage::Record::Missing,
            Self::TooLarge => storage::Record::Malformed(storage::MalformedRecord::TooLarge {
                limit: storage::RECORD_SIZE_LIMIT,
            }),
            Self::Invalid(mut source) => {
                if contest {
                    source.kind = "contest_unreadable";
                }
                return Err(source);
            }
            Self::Present(bytes) => storage::classify_user_record(&bytes, 1, |document| {
                if contest {
                    crate::add::contest_members(document)?;
                } else {
                    crate::selection::problem_identity(document)?;
                }
                Ok::<_, String>(document.clone())
            }),
        };
        read::record(path.to_owned(), record, contest)
    }
}

pub(super) fn workspace(
    root: &Path,
    args: &SearchArgs,
    cancel: &CancelState,
    shared: &Mutex<Results>,
) -> Result<(), Error> {
    let mut budget = Budget::new(cancel, shared);
    match traverse(root, args, &mut budget) {
        Ok(result) => result,
        Err(Stop) => Ok(()),
    }
}

fn traverse(
    root: &Path,
    args: &SearchArgs,
    budget: &mut Budget<'_>,
) -> Result<Result<(), Error>, Stop> {
    let zone = crate::organisation_dates::zone(&args.filters);
    let inventory = match budget.inventory(root, "problems")? {
        Ok(value) => value,
        Err(error) => return Ok(Err(error)),
    };
    let mut contests = Vec::new();
    if !args.filters.contest.is_empty() {
        let inventory = match budget.inventory(root, "contests")? {
            Ok(value) => value,
            Err(error) => return Ok(Err(error)),
        };
        for name in &args.filters.contest {
            if contests
                .iter()
                .any(|contest: &Contest| contest.name == *name)
            {
                continue;
            }
            let found = inventory
                .entries
                .iter()
                .find(|entry| entry.name == name.as_str());
            let Some(entry) = found else {
                return Ok(Err(Error::Workspace(format!(
                    "contest {name}: no valid contest"
                ))));
            };
            if let Err(source) = inventory.validate(entry, "contests") {
                return Ok(Err(Error::Workspace(format!(
                    "contest {name}: {}",
                    source.message
                ))));
            }
            let path = Path::new("contests").join(name).join("contest.toml");
            let document = match budget.file(root, &path, false)?.document(&path, true) {
                Ok(document) => document,
                Err(source) => {
                    return Ok(Err(Error::Workspace(format!(
                        "contest {name}: {}",
                        source.message
                    ))));
                }
            };
            let id = Identifier::new(name.clone()).expect("argument validated");
            if let Err(error) = crate::selection::contest_binding(&document, &id) {
                return Ok(Err(Error::Workspace(format!("contest {name}: {error}"))));
            }
            let mut result = budget.shared.lock().unwrap();
            let findings = organisation::tags(document.get("tags"))
                .err()
                .map(|reason| organisation::Finding {
                    field: "tags",
                    reason,
                })
                .into_iter()
                .collect();
            let sources = read::field_sources(path, findings);
            result.reasons[3] |= !sources.is_empty();
            result.findings.extend(sources);
            contests.push(Contest {
                name: name.clone(),
                document,
            });
        }
    }
    let terms = args
        .terms
        .iter()
        .map(|term| names::collision_key(term))
        .collect::<Vec<_>>();
    for entry in &inventory.entries {
        budget.stopped()?;
        let id = match inventory.validate(entry, "problems") {
            Ok(id) => id,
            Err(source) => {
                publish(budget, None, vec![source], false);
                continue;
            }
        };
        let directory = Path::new("problems").join(&entry.name);
        let path = directory.join("problem.toml");
        let document = match budget.file(root, &path, false)?.document(&path, false) {
            Ok(document) => document,
            Err(source) => {
                publish(budget, None, vec![source], false);
                continue;
            }
        };
        let identity = ProblemIdentity::parse(
            &crate::selection::problem_identity(&document).expect("validated identity"),
        )
        .expect("validated identity");
        if let Err(reason) = identity.check_binding(&id) {
            publish(
                budget,
                None,
                vec![source(
                    &path,
                    "identity_mismatch",
                    format!("{reason:?}: {identity}"),
                )],
                false,
            );
            continue;
        }
        let problem = Problem {
            selected: crate::selection::Problem {
                dir: root.join(&directory),
                identity,
            },
            document,
        };
        let mut findings = read::field_sources(path, organisation::findings(problem.fields()));
        if !read::matches(&args.filters, &problem, &contests, &zone) {
            publish(budget, None, findings, false);
            continue;
        }
        let path = directory.join("notes.md");
        let mut truncated = false;
        let notes = if args.no_notes {
            None
        } else {
            match budget.file(root, &path, true)? {
                FileBytes::Missing | FileBytes::TooLarge => None,
                FileBytes::Invalid(source) => {
                    findings.push(source);
                    None
                }
                FileBytes::Present(mut bytes) => {
                    truncated = bytes.len() > NOTES_LIMIT;
                    if truncated {
                        bytes.truncate(NOTES_LIMIT);
                    }
                    // Drop only an incomplete trailing sequence at the cap.
                    if truncated {
                        let mut offset = 0;
                        while let Err(error) = std::str::from_utf8(&bytes[offset..]) {
                            offset += error.valid_up_to();
                            if let Some(length) = error.error_len() {
                                offset += length;
                            } else {
                                bytes.truncate(offset);
                                break;
                            }
                        }
                    }
                    if std::str::from_utf8(&bytes).is_err() {
                        findings.push(source(&path, "notes_not_utf8", "invalid UTF-8 replaced"));
                    }
                    Some(String::from_utf8_lossy(&bytes).into_owned())
                }
            }
        };
        let hit = match_problem(&problem, notes.as_deref(), truncated, &terms);
        publish(budget, hit, findings, truncated);
    }
    Ok(Ok(()))
}

fn publish(budget: &Budget<'_>, hit: Option<super::Hit>, findings: Vec<Source>, truncated: bool) {
    // No I/O under this lock: cancellation can snapshot immediately even if
    // the worker's next system call never returns.
    let mut result = budget.shared.lock().unwrap();
    if budget.cancel.cancellation().is_some() {
        return;
    }
    result.reasons[2] |= truncated;
    result.reasons[3] |= findings
        .iter()
        .any(|finding| finding.kind != "notes_not_utf8");
    result.findings.extend(findings);
    if let Some(hit) = hit {
        result.hits.push(hit);
    }
}
