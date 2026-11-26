//! Job storage foundations (§6): identity, registration metadata and lease lifetime.
//!
//! These primitives do not admit or prepare a job for execution. Admission must
//! hold one workspace guard continuously through pruning, sequence allocation,
//! directory creation, metadata publication and lease acquisition. Callers check
//! tool-owned paths through that guard; probes used by pruning also run under it.
//! No age or PID is evidence that a lease is inactive.

use std::fmt;
use std::fs::File;
use std::io;
use std::path::Path;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use toml_edit::DocumentMut;

use super::budget::JobBudget;
pub use super::job_seq::Epoch;
use super::job_seq::parse_decimal_u64;
use super::{
    Published, Record, StorageError, WorkspaceGuard, classify_tool_record, publish_file_noclobber,
    read_tool_record, sys,
};

/// One portable path component: 17 UTC timestamp digits, `-`, 16 lowercase hex.
/// Ordering is string byte order (time, then random tie-breaker). Clock rollback
/// can change pruning order; this identity is not an admission sequence.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct JobId(String);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InvalidJobId;

impl fmt::Display for InvalidJobId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("job id must be 17 ASCII digits, '-', and 16 lowercase hex digits")
    }
}

impl std::error::Error for InvalidJobId {}

impl JobId {
    /// Generate an id at the supplied UTC instant, truncating to milliseconds.
    /// Years outside 0000..=9999 and randomness failures are errors. Callers use
    /// `create_dir` and generate again on collision; an id never reuses a job.
    pub fn generate(now: SystemTime) -> io::Result<Self> {
        #[cfg(feature = "storage-test-hooks")]
        if let Ok(id) = std::env::var("CPF_STORAGE_FIXED_JOB_ID") {
            return Self::parse(&id).map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e));
        }
        let timestamp = utc_timestamp(now)?;
        let mut random = [0_u8; 8];
        getrandom::fill(&mut random).map_err(|error| io::Error::other(error.to_string()))?;
        Ok(Self(format!(
            "{timestamp}-{:016x}",
            u64::from_be_bytes(random)
        )))
    }

    /// Strict lexical validation only: each timestamp position accepts 0..=9.
    /// Calendar ranges (including month/day/hour) are deliberately not checked.
    /// The exact ASCII shape excludes separators, dot components and prefixes.
    pub fn parse(value: &str) -> Result<Self, InvalidJobId> {
        let bytes = value.as_bytes();
        if bytes.len() != 34
            || !bytes[..17].iter().all(u8::is_ascii_digit)
            || bytes[17] != b'-'
            || !bytes[18..]
                .iter()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(byte))
        {
            return Err(InvalidJobId);
        }
        Ok(Self(value.to_owned()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn reclaim_name(&self) -> String {
        format!(".reclaim-{}", self.as_str())
    }

    /// Recognize only the exact reclaim prefix and a complete valid job id.
    pub fn parse_reclaim_name(name: &str) -> Result<Self, InvalidJobId> {
        Self::parse(name.strip_prefix(".reclaim-").ok_or(InvalidJobId)?)
    }
}

impl fmt::Display for JobId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

fn utc_timestamp(now: SystemTime) -> io::Result<String> {
    // Signed nanoseconds let pre-epoch instants round down to the millisecond,
    // just like positive instants, rather than truncating toward the epoch.
    let nanos = match now.duration_since(UNIX_EPOCH) {
        Ok(duration) => duration.as_nanos() as i128,
        Err(error) => -(error.duration().as_nanos() as i128),
    };
    let millis = nanos.div_euclid(1_000_000);
    // Gregorian days from 0000-01-01 to 1970-01-01, including leap year 0000.
    let mut days = millis.div_euclid(86_400_000) + 719_528;
    // Exactly 10,000 years (25 complete Gregorian 400-year cycles).
    if !(0..3_652_425).contains(&days) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "job timestamp year must be in 0000..=9999",
        ));
    }
    let mut year = (days / 146_097) * 400;
    days %= 146_097;
    loop {
        let year_days = if leap_year(year) { 366 } else { 365 };
        if days < year_days {
            break;
        }
        days -= year_days;
        year += 1;
    }
    let month_days = [
        31,
        if leap_year(year) { 29 } else { 28 },
        31,
        30,
        31,
        30,
        31,
        31,
        30,
        31,
        30,
        31,
    ];
    let mut month = 1;
    for length in month_days {
        if days < length {
            break;
        }
        days -= length;
        month += 1;
    }
    let daytime = millis.rem_euclid(86_400_000);
    let hour = daytime / 3_600_000;
    let minute = daytime / 60_000 % 60;
    let second = daytime / 1_000 % 60;
    let milli = daytime % 1_000;
    let day = days + 1;
    Ok(format!(
        "{year:04}{month:02}{day:02}{hour:02}{minute:02}{second:02}{milli:03}"
    ))
}

fn leap_year(year: i128) -> bool {
    year % 4 == 0 && (year % 100 != 0 || year % 400 == 0)
}

/// Schema-1 registration metadata. The schema is fixed by this type, so every
/// constructed value encodes as schema 1. Capture state belongs to a ready job.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct JobRecord {
    pub epoch: Epoch,
    pub n: u64,
}

impl JobRecord {
    pub const SCHEMA: i64 = 1;

    /// Canonical bytes, including the final newline and §9 decimal string.
    pub fn encode(&self) -> Vec<u8> {
        format!(
            "schema = 1\nepoch = \"{}\"\nn = \"{}\"\n",
            self.epoch, self.n
        )
        .into_bytes()
    }
}

/// Require an integer schema before decoding any registration fields.
pub fn classify_job_record(bytes: &[u8]) -> Record<JobRecord, &'static str> {
    classify_tool_record(bytes, JobRecord::SCHEMA, validate_record)
}

/// Bounded (1 MiB) tool-only reader for a checked `job.toml` path.
pub fn read_job_record(path: impl AsRef<Path>) -> Record<JobRecord, &'static str> {
    read_tool_record(path, JobRecord::SCHEMA, validate_record)
}

fn validate_record(document: &DocumentMut) -> Result<JobRecord, &'static str> {
    if document
        .iter()
        .any(|(key, _)| !matches!(key, "schema" | "epoch" | "n"))
    {
        return Err("unknown job.toml key");
    }
    let epoch = document
        .get("epoch")
        .and_then(|item| item.as_str())
        .ok_or("epoch must be exactly 32 lowercase hex digits")?;
    let epoch = Epoch::parse(epoch)?;
    let n = document
        .get("n")
        .and_then(|item| item.as_str())
        .and_then(parse_decimal_u64)
        .ok_or("n must be a canonical decimal string no greater than u64::MAX")?;
    Ok(JobRecord { epoch, n })
}

/// Publish registration metadata without replacing an existing `job.toml`.
/// Borrow the admission guard; this never acquires another workspace lock.
/// The primitive's commit, durability and cleanup outcomes are preserved.
pub fn publish_job_record(
    guard: &WorkspaceGuard<'_>,
    job_dir: impl AsRef<Path>,
    record: &JobRecord,
) -> Result<Published, StorageError> {
    let published =
        publish_file_noclobber(guard, job_dir.as_ref().join("job.toml"), &record.encode())?;
    super::crash::point("job.record_published");
    Ok(published)
}

/// An exclusively locked lease. The file handle is private and never cloned or
/// explicitly unlocked while an owner clone lives. Handles are non-inheritable.
#[derive(Debug)]
pub struct JobLease {
    directory: std::path::PathBuf,
    file: File,
    budget: Option<Arc<JobBudget>>,
}

/// Cheap shared ownership of one lease handle. Only the last clone's drop
/// releases the lock and the registered job's retained-byte accounting.
pub type JobOwner = Arc<JobLease>;

impl JobLease {
    pub fn directory(&self) -> &Path {
        &self.directory
    }

    /// Exclusively create and immediately lock `lease` in an existing checked
    /// job directory. Admission calls this while still holding its workspace
    /// guard. Any failure leaves interrupted storage for later reclamation.
    pub fn create_and_lock(job_dir: impl AsRef<Path>) -> io::Result<JobOwner> {
        Self::create_and_lock_inner(job_dir.as_ref(), None)
    }

    pub(crate) fn create_and_lock_with_budget(
        job_dir: &Path,
        budget: Arc<JobBudget>,
    ) -> io::Result<JobOwner> {
        Self::create_and_lock_inner(job_dir, Some(budget))
    }

    /// Registered owners and all their clones share the same accounting.
    /// Standalone lease primitives and independent probes have no budget.
    pub fn budget(&self) -> Option<&Arc<JobBudget>> {
        self.budget.as_ref()
    }

    fn create_and_lock_inner(
        job_dir: &Path,
        budget: Option<Arc<JobBudget>>,
    ) -> io::Result<JobOwner> {
        let file = sys::open_lease(&job_dir.join("lease"), true)?;
        super::crash::point("admission.lease_created");
        if !sys::try_lock(&file)? {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "new job lease is already locked",
            ));
        }
        Ok(Arc::new(Self {
            directory: job_dir.to_owned(),
            file,
            budget,
        }))
    }
}

impl Drop for JobLease {
    fn drop(&mut self) {
        sys::unlock(&self.file);
        if let Some(budget) = &self.budget {
            budget.owner_released();
        }
    }
}

/// An acquired probe keeps its independent handle locked until dropped. Pruning
/// must drop it before the directory rename, closing the handle for Windows.
/// It cannot become a job owner; a free lease is never re-taken by an owner.
#[derive(Debug)]
pub struct ProbeLock {
    _lease: JobLease,
}

#[derive(Debug)]
pub enum ProbeOutcome {
    Held,
    Acquired(ProbeLock),
    Absent,
    /// Cannot establish inactivity. Preserve the directory and report the error.
    Failed(io::Error),
}

/// Independently open and nonblockingly probe a checked job directory's lease.
/// No file is created, no owner handle is reused, and I/O errors are never
/// evidence of inactivity. A missing lease denotes interrupted admission.
pub fn probe(job_dir: impl AsRef<Path>) -> ProbeOutcome {
    let file = match sys::open_lease(&job_dir.as_ref().join("lease"), false) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return ProbeOutcome::Absent,
        Err(error) => return ProbeOutcome::Failed(error),
    };
    match sys::try_lock(&file) {
        Ok(true) => ProbeOutcome::Acquired(ProbeLock {
            _lease: JobLease {
                directory: job_dir.as_ref().to_path_buf(),
                file,
                budget: None,
            },
        }),
        Ok(false) => ProbeOutcome::Held,
        Err(error) => ProbeOutcome::Failed(error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn generated_ids_format_utc_calendar_and_milliseconds() {
        for (seconds, timestamp) in [
            (0_i64, "19700101000000"),
            (-1, "19691231235959"),
            (-2_203_891_200, "19000301000000"),
            (951_782_400, "20000229000000"),
            (951_868_800, "20000301000000"),
            (1_709_164_800, "20240229000000"),
            (4_107_542_400, "21000301000000"),
            (253_402_214_400, "99991231000000"),
        ] {
            let now = if seconds >= 0 {
                UNIX_EPOCH + Duration::from_secs(seconds as u64)
            } else {
                UNIX_EPOCH - Duration::from_secs((-seconds) as u64)
            };
            let now = now + Duration::from_micros(123_999);
            let id = JobId::generate(now).unwrap();
            assert_eq!(&id.as_str()[..17], format!("{timestamp}123"));
            assert_eq!(JobId::parse(id.as_str()).unwrap(), id);
            assert_eq!(Path::new(id.as_str()).components().count(), 1);
        }
        // Windows SystemTime has 100 ns precision.
        let before_epoch = UNIX_EPOCH - Duration::from_nanos(100);
        assert_eq!(utc_timestamp(before_epoch).unwrap(), "19691231235959999");
        assert!(utc_timestamp(UNIX_EPOCH + Duration::from_secs(253_402_300_800)).is_err());
        // Some platforms cannot represent all early Gregorian instants.
        if let Some(year_zero) = UNIX_EPOCH.checked_sub(Duration::from_secs(62_167_219_200)) {
            assert_eq!(utc_timestamp(year_zero).unwrap(), "00000101000000000");
            if let Some(earlier) = year_zero.checked_sub(Duration::from_millis(1)) {
                assert!(utc_timestamp(earlier).is_err());
            }
        }
    }

    #[test]
    fn id_parse_vectors_and_reclaim_round_trip() {
        for value in [
            "20261120230104123-0123456789abcdef",
            "00000000000000000-0000000000000000",
            "99999999999999999-ffffffffffffffff",
        ] {
            let id = JobId::parse(value).unwrap();
            assert_eq!(id.as_str(), value);
            assert_eq!(JobId::parse_reclaim_name(&id.reclaim_name()).unwrap(), id);
        }
        for value in [
            "",
            "2026112023010412-0123456789abcdef",
            "202611202301041234-0123456789abcdef",
            "20261120230104123_0123456789abcdef",
            "20261120230104123-0123456789abcde",
            "20261120230104123-0123456789abcdef0",
            "20261120230104123-0123456789abcdeF",
            "20261120230104123-0123456789abcdeg",
            "20261120230104a23-0123456789abcdef",
            "2026112023010412١-0123456789abcdef",
            "20261120230104123-0123456789abcdeｆ",
            "20261120230104123/0123456789abcdef",
            "20261120230104123-0123456789abcde\\",
            "20261120230104123-0123456789abcde:",
            "20261120230104123-0123456789abcde\0",
            "20261120230104123-0123456789abcdef\n",
            ".",
            "..",
        ] {
            assert!(JobId::parse(value).is_err(), "{value:?}");
        }
        for name in [
            "20261120230104123-0123456789abcdef",
            ".reclaim-",
            ".reclaim-.reclaim-20261120230104123-0123456789abcdef",
            ".reclaim-20261120230104123-0123456789abcdef/extra",
            ".reclaim-20261120230104123-0123456789abcdeF",
        ] {
            assert!(JobId::parse_reclaim_name(name).is_err(), "{name}");
        }
    }

    #[test]
    fn id_order_is_time_then_random_byte_order() {
        let values = [
            "20261120230104122-ffffffffffffffff",
            "20261120230104123-0000000000000000",
            "20261120230104123-0000000000000009",
            "20261120230104123-000000000000000a",
            "20261120230104123-ffffffffffffffff",
            "20261120230104124-0000000000000000",
        ];
        let mut ids: Vec<_> = values
            .iter()
            .rev()
            .map(|s| JobId::parse(s).unwrap())
            .collect();
        ids.sort();
        assert_eq!(ids.iter().map(JobId::as_str).collect::<Vec<_>>(), values);
    }

    #[test]
    fn owner_and_acquired_probe_handles_disable_inheritance() {
        fn assert_non_inheritable(file: &File) {
            #[cfg(unix)]
            {
                use std::os::fd::AsRawFd;
                // SAFETY: the descriptor is borrowed and live; F_GETFD only
                // reads descriptor flags and takes no extra argument.
                let flags = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_GETFD) };
                assert_ne!(flags, -1);
                assert_ne!(flags & libc::FD_CLOEXEC, 0);
            }
            #[cfg(windows)]
            {
                use std::os::windows::io::AsRawHandle;
                use windows_sys::Win32::Foundation::{GetHandleInformation, HANDLE_FLAG_INHERIT};
                let mut flags = 0;
                // SAFETY: the handle is borrowed and live; flags is writable.
                assert_ne!(
                    unsafe { GetHandleInformation(file.as_raw_handle(), &mut flags) },
                    0
                );
                assert_eq!(flags & HANDLE_FLAG_INHERIT, 0);
            }
        }
        let id = JobId::generate(SystemTime::now()).unwrap();
        let root = std::env::temp_dir().join(format!("cpforge-lease-{id}"));
        std::fs::create_dir(&root).unwrap();
        let owner = JobLease::create_and_lock(&root).unwrap();
        assert_non_inheritable(&owner.file);
        drop(owner);
        // Other unit tests spawn processes concurrently; on Unix a child shares
        // the open file description between fork and exec, so retry `Held`.
        let start = std::time::Instant::now();
        let lock = loop {
            match probe(&root) {
                ProbeOutcome::Acquired(lock) => break lock,
                ProbeOutcome::Held if start.elapsed() < Duration::from_secs(10) => {
                    std::thread::sleep(Duration::from_millis(10));
                }
                outcome => panic!("expected acquired probe, got {outcome:?}"),
            }
        };
        assert_non_inheritable(&lock._lease.file);
        drop(lock);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn windows_rename_needs_every_handle_in_the_job_directory_closed() {
        use std::fs;
        let id = JobId::generate(SystemTime::now()).unwrap();
        let root = std::env::temp_dir().join(format!("cpforge-lease-{id}"));
        fs::create_dir(&root).unwrap();
        let original = root.join("job");
        fs::create_dir(&original).unwrap();
        let owner = JobLease::create_and_lock(&original).unwrap();
        // Owner and an independent probe handle coexist (compatible sharing),
        // and the probe sees the owner's lock.
        let independent = sys::open_lease(&original.join("lease"), false).unwrap();
        assert!(!sys::try_lock(&independent).unwrap());
        // Windows refuses to rename a directory while a file inside is open,
        // whatever the sharing mode; pruning therefore closes its probe first.
        let renamed = root.join("renamed");
        assert!(fs::rename(&original, &renamed).is_err());
        drop(independent);
        assert!(fs::rename(&original, &renamed).is_err());
        drop(owner);
        let lock = match super::probe(&original) {
            ProbeOutcome::Acquired(lock) => lock,
            other => panic!("expected an inactive lease, got {other:?}"),
        };
        assert!(fs::rename(&original, &renamed).is_err());
        drop(lock);
        fs::rename(&original, &renamed).unwrap();
        assert!(matches!(super::probe(&renamed), ProbeOutcome::Acquired(_)));
        fs::remove_dir_all(root).unwrap();
    }
}
