use std::cell::RefCell;
use std::collections::HashMap;
use std::fs::{self, File, OpenOptions};
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, TryLockError};
use std::time::{Duration, Instant};

use super::{ErrorKind, RecoveryIncomplete, StorageError, recovery, sys};

#[derive(Clone, Debug)]
pub struct LockOptions {
    /// The whole acquisition (local mutex and OS lock) shares this bound.
    /// Values above the contract's ten seconds are capped.
    pub timeout: Duration,
    #[cfg(feature = "storage-test-hooks")]
    pub test_hooks: super::TestHooks,
}

impl Default for LockOptions {
    fn default() -> Self {
        Self {
            timeout: Duration::from_secs(10),
            #[cfg(feature = "storage-test-hooks")]
            test_hooks: super::TestHooks::default(),
        }
    }
}

#[derive(Debug, Default)]
struct State {
    file: Option<File>,
}

type Registry = HashMap<PathBuf, Arc<Mutex<State>>>;
static LOCKS: OnceLock<Mutex<Registry>> = OnceLock::new();

/// Handles for the same canonical root share one local mutex and one OS handle.
/// The registry retains the handle until its directory entry is replaced.
#[derive(Clone, Debug)]
pub struct WorkspaceLock {
    root: PathBuf,
    state: Arc<Mutex<State>>,
}

impl WorkspaceLock {
    /// Canonicalize the root once; a link used to reach the root is allowed.
    pub fn new(root: impl AsRef<Path>) -> Result<Self, StorageError> {
        let root = fs::canonicalize(root.as_ref())
            .map(|path| super::paths::plain(&path))
            .map_err(|e| StorageError::io(root.as_ref(), e))?;
        if !fs::metadata(&root)
            .map_err(|e| StorageError::io(&root, e))?
            .is_dir()
        {
            return Err(StorageError::new(ErrorKind::InvalidPath { path: root }));
        }
        let mut registry = LOCKS
            .get_or_init(Mutex::default)
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        let state = Arc::clone(registry.entry(root.clone()).or_default());
        Ok(Self { root, state })
    }

    pub fn acquire(&self, op: impl Into<String>) -> Result<WorkspaceGuard<'_>, StorageError> {
        self.acquire_with_options(op, LockOptions::default())
    }

    pub fn acquire_with_options(
        &self,
        op: impl Into<String>,
        options: LockOptions,
    ) -> Result<WorkspaceGuard<'_>, StorageError> {
        let op = op.into();
        let start = Instant::now();
        let bound = options.timeout.min(Duration::from_secs(10));
        let mut delay = Duration::from_millis(1);
        let mut state = loop {
            match self.state.try_lock() {
                Ok(state) => break state,
                Err(TryLockError::Poisoned(error)) => break error.into_inner(),
                Err(TryLockError::WouldBlock) => {
                    if !backoff(start, bound, &mut delay) {
                        return Err(StorageError::new(ErrorKind::Busy { op }));
                    }
                }
            }
        };
        let mut created = Vec::new();
        let lock_path = self.root.join(".cpforge/lock");
        loop {
            // Check even cached handles: structural directories may have changed
            // between outer operations in a long-lived process.
            checked_path(&self.root, &lock_path, false)?;
            if state.file.is_none() {
                let metadata = checked_path(&self.root, Path::new(".cpforge"), false)?;
                match fs::create_dir(&metadata) {
                    Ok(()) => created.push(metadata.clone()),
                    Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                        if !fs::symlink_metadata(&metadata)
                            .map_err(|e| StorageError::io(&metadata, e))?
                            .is_dir()
                        {
                            return Err(StorageError::new(ErrorKind::InvalidPath {
                                path: metadata,
                            }));
                        }
                    }
                    Err(error) => return Err(StorageError::io(&metadata, error)),
                }
                checked_path(&self.root, &lock_path, false)?;
                let mut open = OpenOptions::new();
                open.read(true).write(true).create(true).truncate(false);
                #[cfg(unix)]
                {
                    use std::os::unix::fs::OpenOptionsExt;
                    open.custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK);
                }
                let file = open
                    .open(&lock_path)
                    .map_err(|e| StorageError::io(&lock_path, e))?;
                if !file
                    .metadata()
                    .map_err(|e| StorageError::io(&lock_path, e))?
                    .is_file()
                {
                    return Err(StorageError::new(ErrorKind::InvalidPath {
                        path: lock_path,
                    }));
                }
                #[cfg(windows)]
                {
                    use std::os::windows::io::AsRawHandle;
                    use windows_sys::Win32::Foundation::{
                        HANDLE_FLAG_INHERIT, SetHandleInformation,
                    };
                    // SAFETY: the owned file handle is live; clearing inheritance does not change ownership.
                    if unsafe { SetHandleInformation(file.as_raw_handle(), HANDLE_FLAG_INHERIT, 0) }
                        == 0
                    {
                        return Err(StorageError::io(
                            &lock_path,
                            std::io::Error::last_os_error(),
                        ));
                    }
                }
                state.file = Some(file);
            }
            if sys::try_lock(state.file.as_ref().expect("opened lock"))
                .map_err(|e| StorageError::io(&lock_path, e))?
            {
                let file = state.file.as_ref().expect("opened lock");
                let identity = sys::same_file(file, &lock_path);
                if matches!(identity, Ok(true)) {
                    break;
                }
                sys::unlock(file);
                state.file = None;
                identity.map_err(|e| StorageError::io(&lock_path, e))?;
            }
            if !backoff(start, bound, &mut delay) {
                return Err(StorageError::new(ErrorKind::Busy { op }));
            }
        }
        let mut guard = WorkspaceGuard {
            root: &self.root,
            state,
            created: RefCell::new(created),
            recovery: None,
            #[cfg(feature = "storage-test-hooks")]
            test_hooks: options.test_hooks,
        };
        // Bootstrap of the metadata directory and lock is the only creation
        // before recovery: both are needed to serialize first-time initialization.
        super::crash::point("lock.acquired");
        guard.recovery = recovery::recover(&guard);
        super::crash::point("lock.recovered");
        Ok(guard)
    }
}

/// Not reentrant, Send or Sync. Internal steps borrow this guard and never lock.
#[derive(Debug)]
pub struct WorkspaceGuard<'a> {
    root: &'a Path,
    state: MutexGuard<'a, State>,
    pub(crate) created: RefCell<Vec<PathBuf>>,
    recovery: Option<RecoveryIncomplete>,
    #[cfg(feature = "storage-test-hooks")]
    pub(crate) test_hooks: super::TestHooks,
}

impl WorkspaceGuard<'_> {
    pub fn root(&self) -> &Path {
        self.root
    }

    pub fn recovery_warning(&self) -> Option<&RecoveryIncomplete> {
        self.recovery.as_ref()
    }

    pub(crate) fn take_recovery_warning(&mut self) -> Option<RecoveryIncomplete> {
        self.recovery.take()
    }

    /// Resolve a path confined below the root, checking every existing component
    /// at use. Missing final components are permitted for exclusive creation.
    /// Absolute paths must be constructed from `guard.root()` (the canonical
    /// root); alternate spellings through root links or platform aliases are refused.
    pub fn check_path(&self, path: impl AsRef<Path>) -> Result<PathBuf, StorageError> {
        checked_path(self.root, path.as_ref(), false)
    }

    pub(crate) fn entry_path(&self, path: &Path) -> Result<PathBuf, StorageError> {
        checked_path(self.root, path, true)
    }

    /// Create structural directories individually and remember the new ancestors
    /// for the publication's bottom-up durability syncs. Names use §2.1 policy.
    pub fn create_dir_all(&self, path: impl AsRef<Path>) -> Result<PathBuf, StorageError> {
        let target = self.check_path(path)?;
        let relative = target.strip_prefix(self.root).expect("checked root");
        let mut current = self.root.to_path_buf();
        for component in relative.components() {
            crate::names::PortableComponent::from_os_str(component.as_os_str())
                .map_err(|e| StorageError::new(ErrorKind::InvalidName(e)))?;
            current.push(component);
            self.check_path(&current)?;
            match fs::create_dir(&current) {
                Ok(()) => self.created.borrow_mut().push(current.clone()),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                    if !fs::symlink_metadata(&current)
                        .map_err(|e| StorageError::io(&current, e))?
                        .is_dir()
                    {
                        return Err(StorageError::new(ErrorKind::InvalidPath { path: current }));
                    }
                }
                Err(error) => return Err(StorageError::io(&current, error)),
            }
        }
        Ok(target)
    }
}

impl Drop for WorkspaceGuard<'_> {
    fn drop(&mut self) {
        sys::unlock(self.state.file.as_ref().expect("opened lock"));
    }
}

pub(crate) fn checked_path(
    root: &Path,
    path: &Path,
    allow_final_link: bool,
) -> Result<PathBuf, StorageError> {
    let relative = if path.is_absolute() {
        path.strip_prefix(root)
            .map_err(|_| StorageError::new(ErrorKind::InvalidPath { path: path.into() }))?
    } else {
        path
    };
    let components: Vec<_> = relative.components().collect();
    let mut current = root.to_path_buf();
    for (index, component) in components.iter().enumerate() {
        let Component::Normal(name) = component else {
            return Err(StorageError::new(ErrorKind::InvalidPath {
                path: path.into(),
            }));
        };
        current.push(name);
        match fs::symlink_metadata(&current) {
            Ok(metadata)
                if sys::is_link(&metadata)
                    && !(allow_final_link && index + 1 == components.len()) =>
            {
                return Err(StorageError::new(ErrorKind::LinkInPath { path: current }));
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(StorageError::io(&current, error)),
        }
    }
    Ok(current)
}

pub(crate) fn backoff(start: Instant, bound: Duration, delay: &mut Duration) -> bool {
    let Some(remaining) = bound.checked_sub(start.elapsed()).filter(|d| !d.is_zero()) else {
        return false;
    };
    std::thread::sleep((*delay).min(remaining));
    *delay = (*delay * 2).min(Duration::from_millis(100));
    start.elapsed() < bound
}
