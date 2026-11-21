//! Job retention (§6.3): select and rename with a borrowed workspace guard,
//! then drop that guard before calling [`reclaim`]. No age, PID or directory
//! timestamp is evidence of inactivity. Admission and `clean` own orchestration.
//!
//! Policy interpretations: known inactive, protected jobs count for `keep_jobs`;
//! every name beginning `.reclaim-` belongs to step 3, including malformed id
//! suffixes. JSON schema reads reuse the small-record reader's 1 MiB bound and
//! oversize classification. Governed metadata is recognised only at its §6.1
//! layout positions (`<job>/job.toml`, `<job>/report.json`,
//! `<job>/<run>/spec.json`, `<job>/<run>/record.json`): files of those names
//! deeper down (captured sources, child output in `cwd/`) are not records and
//! must not pin a job.

use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use super::job::{JobId, ProbeOutcome, probe, read_job_record};
use super::{ErrorKind, Record, RecordBytes, StorageError, WalkReport, WorkspaceGuard};
use super::{checked_path, crash, read_record_bytes, remove_owned_tree, sys, walk};

/// Storage policy only; user-config validation (including `keep_jobs >= 1`)
/// belongs to configuration. Byte usage is a soft target, never admission denial.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PrunePolicy {
    pub keep_jobs: u64,
    pub workspace_bytes: u64,
}

impl PrunePolicy {
    pub const fn new(keep_jobs: u64, workspace_bytes: u64) -> Self {
        Self {
            keep_jobs,
            workspace_bytes,
        }
    }

    /// Internal `clean --apply` policy; not a user-config default.
    pub const fn zero() -> Self {
        Self::new(0, 0)
    }
}

impl Default for PrunePolicy {
    fn default() -> Self {
        Self::new(20, 4 << 30)
    }
}

#[derive(Debug)]
pub struct ProtectedMetadata {
    pub job: JobId,
    pub path: PathBuf,
    pub schema: u64,
}

#[derive(Debug)]
pub struct RenameFailure {
    pub job: JobId,
    pub source: PathBuf,
    pub target: PathBuf,
    /// Includes an existing target, which is never overwritten.
    pub error: StorageError,
}

/// All outcomes are warnings: pruning never refuses admission for usage.
#[derive(Debug, Default)]
pub struct PruneReport {
    /// Logical bytes of all of `runs/`, including active jobs, reclaim leftovers
    /// and non-job entries. Errors/skips describe the unknown portion.
    pub usage: WalkReport,
    pub incomplete_measurement: bool,
    /// Known usage less the sizes of successful renames only. This is projected
    /// usage after reclamation, not the actual usage before step 3.
    pub projected_bytes: u64,
    pub skipped_active: Vec<JobId>,
    pub protected: Vec<ProtectedMetadata>,
    pub failed_probes: Vec<(PathBuf, io::Error)>,
    pub unreadable_metadata: Vec<StorageError>,
    pub failed_renames: Vec<RenameFailure>,
    pub non_job_entries: Vec<PathBuf>,
    /// Path, enumeration or measurement setup failures outside `usage`.
    pub errors: Vec<StorageError>,
}

#[derive(Debug)]
pub struct PruneSelection {
    /// Canonical-root-derived path to pass to `reclaim` after dropping the guard.
    pub runs_dir: PathBuf,
    /// Successfully renamed ids in oldest-first order. Reclaim also retries
    /// leftovers not present in this list.
    pub renamed: Vec<JobId>,
    /// Jobs selected by policy: in a plan, every candidate; otherwise those
    /// that passed the re-probe (renamed or listed in `failed_renames`).
    pub candidates: Vec<JobId>,
    pub report: PruneReport,
}

struct Inactive {
    id: JobId,
    bytes: u64,
    eligible: bool,
}

/// Perform §6.3 steps 1–2 while the caller holds the workspace lock. Known
/// inactive jobs with protected/unreadable metadata still occupy retention
/// slots; jobs whose probes fail cannot be established as inactive.
///
/// A single walker pass measures usage and each immediate child's contribution,
/// and discovers governed metadata at its layout positions (including per-run
/// records).
/// Reads are bounded to 1 MiB. Missing/malformed metadata is disposable; unknown
/// schemas and unreadable metadata are preserved. No handles survive a rename.
pub fn select_and_rename(guard: &WorkspaceGuard<'_>, policy: PrunePolicy) -> PruneSelection {
    select(guard, policy, true)
}

/// Step 1 only, under the caller's workspace lock (the `cpf clean` plan):
/// the same selection, re-probe excepted, with nothing renamed.
pub fn plan(guard: &WorkspaceGuard<'_>, policy: PrunePolicy) -> PruneSelection {
    select(guard, policy, false)
}

fn select(guard: &WorkspaceGuard<'_>, policy: PrunePolicy, apply: bool) -> PruneSelection {
    let mut selection = PruneSelection {
        runs_dir: guard.root().join(".cpforge").join("runs"),
        renamed: Vec::new(),
        candidates: Vec::new(),
        report: PruneReport::default(),
    };
    let runs = &selection.runs_dir;
    let report = &mut selection.report;
    if let Err(error) = guard.check_path(runs) {
        report.incomplete_measurement = true;
        report.errors.push(error);
        return selection;
    }
    let mut metadata: BTreeMap<OsString, Vec<PathBuf>> = BTreeMap::new();
    let measurement = walk::measure_children(guard, runs, &mut |path| {
        if let Some(job) = metadata_job(runs, path) {
            metadata.entry(job.into()).or_default().push(path.into());
        }
    });
    let child_bytes = match measurement {
        Ok((usage, children)) => {
            report.incomplete_measurement = !usage.errors.is_empty() || !usage.skipped.is_empty();
            report.usage = usage;
            children
        }
        Err(error) if missing(&error) => return selection,
        Err(error) => {
            report.incomplete_measurement = true;
            report.errors.push(error);
            BTreeMap::new()
        }
    };
    report.projected_bytes = report.usage.bytes;
    let entries = match fs::read_dir(runs) {
        Ok(entries) => entries,
        Err(error) => {
            report.errors.push(StorageError::io(runs, error));
            return selection;
        }
    };
    let mut jobs = BTreeMap::new();
    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) => {
                report.errors.push(StorageError::io(runs, error));
                continue;
            }
        };
        let name = entry.file_name();
        if is_reclaim(&name) {
            continue; // Step 3 owns every entry in the reserved reclaim prefix.
        }
        let path = entry.path();
        let id = name.to_str().and_then(|name| JobId::parse(name).ok());
        match (id, fs::symlink_metadata(&path)) {
            (Some(id), Ok(info)) if info.is_dir() && !sys::is_link(&info) => {
                jobs.insert(id, path);
            }
            (_, Ok(_)) => report.non_job_entries.push(path),
            (_, Err(error)) => report.errors.push(StorageError::io(path, error)),
        }
    }
    report.non_job_entries.sort();
    let mut inactive = Vec::new();
    for (id, path) in jobs {
        match probe(&path) {
            ProbeOutcome::Held => {
                report.skipped_active.push(id);
                continue;
            }
            ProbeOutcome::Failed(error) => {
                report.failed_probes.push((path, error));
                continue;
            }
            // Drop the independent probe here, well before any rename. Owners
            // only create leases at admission and never re-take a free lease.
            ProbeOutcome::Acquired(lock) => drop(lock),
            ProbeOutcome::Absent => {}
        }
        let mut paths = metadata.remove(OsStr::new(id.as_str())).unwrap_or_default();
        // Also check direct records if measurement failed before reaching them.
        paths.extend(["job.toml", "report.json"].map(|n| path.join(n)));
        let mut eligible = true;
        // A job or run directory that cannot be listed hides records whose
        // schema is unknown, so it keeps the job (§6.3: unreadable record).
        match fs::read_dir(&path) {
            Ok(entries) => {
                for entry in entries {
                    let run = match entry {
                        Ok(entry) => entry.path(),
                        Err(error) => {
                            eligible = false;
                            report
                                .unreadable_metadata
                                .push(StorageError::io(path.clone(), error));
                            continue;
                        }
                    };
                    match fs::symlink_metadata(&run) {
                        Ok(info) if info.is_dir() && !sys::is_link(&info) => {
                            if let Err(error) = fs::read_dir(&run) {
                                eligible = false;
                                report
                                    .unreadable_metadata
                                    .push(StorageError::io(run.clone(), error));
                            }
                            paths.extend(["spec.json", "record.json"].map(|n| run.join(n)));
                        }
                        Ok(_) => {}
                        Err(error) => {
                            eligible = false;
                            report
                                .unreadable_metadata
                                .push(StorageError::io(run, error));
                        }
                    }
                }
            }
            Err(error) => {
                eligible = false;
                report
                    .unreadable_metadata
                    .push(StorageError::io(path.clone(), error));
            }
        }
        paths.sort();
        paths.dedup();
        for path in paths {
            match metadata_schema(guard, &path) {
                Ok(Some(schema)) => {
                    eligible = false;
                    report.protected.push(ProtectedMetadata {
                        job: id.clone(),
                        path,
                        schema,
                    });
                }
                Err(error) => {
                    eligible = false;
                    report.unreadable_metadata.push(error);
                }
                Ok(None) => {}
            }
        }
        let bytes = child_bytes
            .get(OsStr::new(id.as_str()))
            .copied()
            .unwrap_or(0);
        inactive.push(Inactive {
            id,
            bytes,
            eligible,
        });
    }
    let inactive_count = inactive.len() as u64;
    for (index, job) in inactive.into_iter().enumerate() {
        let beyond_keep = (index as u64) < inactive_count.saturating_sub(policy.keep_jobs);
        let beyond_bytes =
            !report.incomplete_measurement && report.projected_bytes > policy.workspace_bytes;
        if !job.eligible || !(beyond_keep || beyond_bytes) {
            continue;
        }
        if !apply {
            selection.candidates.push(job.id.clone());
            report.projected_bytes = report.projected_bytes.saturating_sub(job.bytes);
            continue;
        }
        let source = runs.join(job.id.as_str());
        let target = runs.join(job.id.reclaim_name());
        // Re-probe at the decision's commit boundary. A lock-free independent
        // probe may be holding a formerly free lease briefly.
        match probe(&source) {
            ProbeOutcome::Held => {
                report.skipped_active.push(job.id);
                continue;
            }
            ProbeOutcome::Failed(error) => {
                report.failed_probes.push((source, error));
                continue;
            }
            ProbeOutcome::Acquired(lock) => drop(lock),
            ProbeOutcome::Absent => {}
        }
        selection.candidates.push(job.id.clone());
        let result = rename(guard, &source, &target);
        match result {
            Ok(()) => {
                report.projected_bytes = report.projected_bytes.saturating_sub(job.bytes);
                selection.renamed.push(job.id);
            }
            Err(error) => report.failed_renames.push(RenameFailure {
                job: job.id,
                source,
                target,
                error,
            }),
        }
    }
    selection
}

pub(crate) fn rename(
    guard: &WorkspaceGuard<'_>,
    source: &Path,
    target: &Path,
) -> Result<(), StorageError> {
    guard.check_path(source)?;
    // Check the destination entry without following it, including dangling links.
    match fs::symlink_metadata(target) {
        Ok(_) => {
            return Err(StorageError::new(ErrorKind::Exists {
                path: target.into(),
            }));
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(StorageError::io(target, error)),
    }
    crash::point("prune.before_rename");
    #[cfg(feature = "storage-test-hooks")]
    if guard.test_hooks.fail_prune_rename {
        return Err(StorageError::io(
            source,
            io::Error::new(
                io::ErrorKind::PermissionDenied,
                "injected prune rename failure",
            ),
        ));
    }
    sys::rename_exclusive(source, target).map_err(|error| StorageError::io(source, error))?;
    crash::point("prune.after_rename");
    Ok(())
}

/// The job directory name owning governed metadata at `path`, if `path` is one
/// of the §6.1 record positions below `runs`.
fn metadata_job<'p>(runs: &Path, path: &'p Path) -> Option<&'p OsStr> {
    let relative = path.strip_prefix(runs).ok()?;
    let parts: Vec<&OsStr> = relative.iter().collect();
    match parts.as_slice() {
        [job, name] if *name == "job.toml" || *name == "report.json" => Some(*job),
        [job, _, name] if *name == "spec.json" || *name == "record.json" => Some(*job),
        _ => None,
    }
}

fn metadata_schema(guard: &WorkspaceGuard<'_>, path: &Path) -> Result<Option<u64>, StorageError> {
    guard.check_path(path)?;
    if path.file_name() == Some(OsStr::new("job.toml")) {
        return match read_job_record(path) {
            Record::TooNew { schema, .. } => Ok(Some(schema as u64)),
            Record::Unreadable(error) => Err(StorageError::io(path, error)),
            _ => Ok(None),
        };
    }
    match read_record_bytes(path) {
        RecordBytes::Unreadable(error) => Err(StorageError::io(path, error)),
        RecordBytes::Present(bytes) => {
            // Only schema matters: no spec, record or report fields are decoded.
            let value: JsonSchema = match serde_json::from_slice(&bytes) {
                Ok(value) => value,
                Err(_) => return Ok(None),
            };
            // The three JSON formats currently all support schema 1; their
            // versions are independent of job.toml's registration format.
            Ok(value.schema.filter(|schema| *schema > 1))
        }
        RecordBytes::Missing | RecordBytes::TooLarge => Ok(None),
    }
}

#[derive(serde::Deserialize)]
struct JsonSchema {
    schema: Option<u64>,
}

fn missing(error: &StorageError) -> bool {
    matches!(&error.kind, ErrorKind::Io { source, .. } if source.kind() == io::ErrorKind::NotFound)
}

fn is_reclaim(name: &OsStr) -> bool {
    // The contract reserves the whole prefix for retries, even malformed suffixes.
    name.as_encoded_bytes().starts_with(b".reclaim-")
}

#[derive(Debug, Default)]
pub struct ReclaimReport {
    /// Successfully removed (or concurrently disappeared) entries.
    pub reclaimed: Vec<PathBuf>,
    /// Skipped mount/device boundaries and individual deletion errors.
    pub walk: WalkReport,
    pub errors: Vec<StorageError>,
}

/// Perform §6.3 step 3 without a workspace guard. Pass the absolute `runs_dir`
/// returned by selection; the caller must first release its workspace lock.
/// Retry all `.reclaim-*` entries, of any type, with the shared no-follow walker.
/// Disappearance at any deletion stage is success; other failures leave leftovers.
/// Windows sharing/delete-pending refusals get bounded retries (at most 1.27 s
/// of backoff per entry), with all walker handles closed between attempts.
pub fn reclaim(runs_dir: impl AsRef<Path>) -> ReclaimReport {
    reclaim_impl(
        runs_dir.as_ref(),
        #[cfg(feature = "storage-test-hooks")]
        None,
    )
}

/// Test-only boundary injection into the same unlocked reclaim path.
#[cfg(feature = "storage-test-hooks")]
pub fn reclaim_with_test_hooks(
    runs_dir: impl AsRef<Path>,
    hooks: &super::TestHooks,
) -> ReclaimReport {
    reclaim_impl(runs_dir.as_ref(), Some(hooks))
}

fn reclaim_impl(
    runs: &Path,
    #[cfg(feature = "storage-test-hooks")] hooks: Option<&super::TestHooks>,
) -> ReclaimReport {
    crash::point("prune.before_reclaim");
    let mut report = ReclaimReport::default();
    if !runs.is_absolute() {
        report
            .errors
            .push(StorageError::new(ErrorKind::InvalidPath {
                path: runs.into(),
            }));
        return report;
    }
    // No guard is held now. Recheck structural ancestors, including runs itself,
    // from the volume root; selection supplies a canonical-root-derived path.
    let root = runs.ancestors().last().expect("absolute path");
    if let Err(error) = checked_path(root, runs, false) {
        if !missing(&error) {
            report.errors.push(error);
        }
        return report;
    }
    let entries = match fs::read_dir(runs) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return report,
        Err(error) => {
            report.errors.push(StorageError::io(runs, error));
            return report;
        }
    };
    let mut paths = Vec::new();
    for entry in entries {
        match entry {
            Ok(entry) if is_reclaim(&entry.file_name()) => paths.push(entry.path()),
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => report.errors.push(StorageError::io(runs, error)),
        }
    }
    paths.sort();
    for path in paths {
        match reclaim_tree(
            runs,
            &path,
            #[cfg(feature = "storage-test-hooks")]
            hooks,
        ) {
            Ok(mut walk) => {
                if walk.errors.is_empty() && walk.skipped.is_empty() {
                    report.reclaimed.push(path);
                }
                report.walk.bytes = report.walk.bytes.saturating_add(walk.bytes);
                report.walk.errors.append(&mut walk.errors);
                report.walk.skipped.append(&mut walk.skipped);
            }
            Err(error) if missing(&error) => report.reclaimed.push(path),
            Err(error) => report.errors.push(error),
        }
    }
    report
}

fn reclaim_tree(
    runs: &Path,
    path: &Path,
    #[cfg(feature = "storage-test-hooks")] hooks: Option<&super::TestHooks>,
) -> Result<WalkReport, StorageError> {
    let mut attempt = 0;
    loop {
        #[cfg(feature = "storage-test-hooks")]
        let result = match hooks {
            Some(hooks) => walk::remove_owned_tree_with_hooks(runs, path, hooks),
            None => remove_owned_tree(runs, path),
        };
        #[cfg(not(feature = "storage-test-hooks"))]
        let result = remove_owned_tree(runs, path);
        // Concurrent Windows walks can briefly encounter delete-pending
        // entries or a directory still containing an open, deleted child.
        // The walker has closed every handle before this bounded retry.
        let retry = match &result {
            Ok(walk) => {
                walk.skipped.is_empty() && walk.errors.iter().any(|(_, error)| reclaim_retry(error))
            }
            Err(StorageError {
                kind: ErrorKind::Io { source, .. },
                ..
            }) => reclaim_retry(source),
            Err(_) => false,
        };
        if retry && attempt < 7 {
            std::thread::sleep(std::time::Duration::from_millis(10 << attempt));
            attempt += 1;
            continue;
        }
        return result;
    }
}

fn reclaim_retry(error: &io::Error) -> bool {
    #[cfg(windows)]
    {
        use windows_sys::Win32::Foundation::{
            ERROR_ACCESS_DENIED, ERROR_DELETE_PENDING, ERROR_DIR_NOT_EMPTY, ERROR_SHARING_VIOLATION,
        };
        error.raw_os_error().is_some_and(|code| {
            [
                ERROR_ACCESS_DENIED,
                ERROR_DELETE_PENDING,
                ERROR_DIR_NOT_EMPTY,
                ERROR_SHARING_VIOLATION,
            ]
            .contains(&(code as u32))
        })
    }
    #[cfg(not(windows))]
    {
        let _ = error;
        false
    }
}
