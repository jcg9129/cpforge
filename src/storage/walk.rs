use std::collections::{BTreeMap, HashSet};
use std::ffi::{OsStr, OsString};
use std::fs;
use std::fs::File;
use std::io;
use std::path::{Path, PathBuf};

use super::{ErrorKind, StorageError, WorkspaceGuard, sys};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SkipReason {
    DeviceBoundary,
    MountPoint,
}

#[derive(Debug)]
pub struct Skipped {
    pub path: PathBuf,
    pub reason: SkipReason,
}

/// Logical regular-file bytes (links contribute zero), skipped boundaries and
/// per-entry failures. Deletion keeps every ancestor of a skipped/failed entry.
#[derive(Debug, Default)]
pub struct WalkReport {
    pub bytes: u64,
    pub skipped: Vec<Skipped>,
    pub errors: Vec<(PathBuf, io::Error)>,
}

/// Measure an owned disposable tree. Shared working directories must never be passed here.
pub fn measure_tree(
    guard: &WorkspaceGuard<'_>,
    path: impl AsRef<Path>,
) -> Result<WalkReport, StorageError> {
    run(guard, path.as_ref(), Action::Measure)
}

/// Delete an owned disposable tree, retaining and reporting any skipped boundary.
/// Shared working directories must never be passed here.
pub fn remove_tree(
    guard: &WorkspaceGuard<'_>,
    path: impl AsRef<Path>,
) -> Result<WalkReport, StorageError> {
    let path = guard.entry_path(path.as_ref())?;
    let report = run(guard, &path, Action::Remove)?;
    if report.errors.is_empty() && report.skipped.is_empty() {
        guard
            .created
            .borrow_mut()
            .retain(|created| !created.starts_with(&path));
    }
    Ok(report)
}

/// Per-operation observation inside the actual walker; no global test state.
#[cfg(all(
    feature = "storage-test-hooks",
    any(target_os = "linux", target_os = "macos")
))]
pub(crate) fn measure_tree_observed(
    guard: &WorkspaceGuard<'_>,
    path: &Path,
    on_entry: &mut dyn FnMut(&Path),
) -> Result<WalkReport, StorageError> {
    let path = guard.entry_path(path)?;
    run_owned(
        guard.root(),
        &path,
        Action::Measure,
        Some(&guard.test_hooks),
        None,
        Some(on_entry),
    )
}

#[cfg(all(
    feature = "storage-test-hooks",
    any(target_os = "linux", target_os = "macos")
))]
pub(crate) fn remove_tree_observed(
    guard: &WorkspaceGuard<'_>,
    path: &Path,
    on_entry: &mut dyn FnMut(&Path),
) -> Result<WalkReport, StorageError> {
    let path = guard.entry_path(path)?;
    let report = run_owned(
        guard.root(),
        &path,
        Action::Remove,
        Some(&guard.test_hooks),
        None,
        Some(on_entry),
    )?;
    if report.errors.is_empty() && report.skipped.is_empty() {
        guard
            .created
            .borrow_mut()
            .retain(|created| !created.starts_with(&path));
    }
    Ok(report)
}

pub(crate) fn sync_tree(guard: &WorkspaceGuard<'_>, path: &Path) -> Result<(), StorageError> {
    let report = run(guard, path, Action::Sync)?;
    if let Some((path, error)) = report.errors.into_iter().next() {
        return Err(StorageError::io(path, error));
    }
    if let Some(skipped) = report.skipped.into_iter().next() {
        return Err(StorageError::io(
            skipped.path,
            io::Error::other("staging tree crosses a mount boundary"),
        ));
    }
    Ok(())
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Action {
    Measure,
    Remove,
    Sync,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    File,
    Directory,
    Link,
    Other,
}

struct Info {
    kind: Kind,
    bytes: u64,
    device: u64,
}

struct Walker<'a> {
    _scope: std::marker::PhantomData<&'a ()>,
    #[cfg(feature = "storage-test-hooks")]
    test_hooks: Option<&'a super::TestHooks>,
    action: Action,
    device: u64,
    mounts: HashSet<PathBuf>,
    report: WalkReport,
    child_bytes: Option<(&'a Path, &'a mut BTreeMap<OsString, u64>)>,
    on_entry: Option<&'a mut dyn FnMut(&Path)>,
}

fn run(
    guard: &WorkspaceGuard<'_>,
    path: &Path,
    action: Action,
) -> Result<WalkReport, StorageError> {
    let path = guard.entry_path(path)?;
    run_owned(
        guard.root(),
        &path,
        action,
        #[cfg(feature = "storage-test-hooks")]
        Some(&guard.test_hooks),
        None,
        None,
    )
}

/// One measurement supplies both total usage and immediate-child sizes, with
/// identical no-follow and mount-boundary decisions for both views.
pub(super) fn measure_children(
    guard: &WorkspaceGuard<'_>,
    path: &Path,
    on_entry: &mut dyn FnMut(&Path),
) -> Result<(WalkReport, BTreeMap<OsString, u64>), StorageError> {
    let path = guard.check_path(path)?;
    let mut children = BTreeMap::new();
    let report = run_owned(
        guard.root(),
        &path,
        Action::Measure,
        #[cfg(feature = "storage-test-hooks")]
        Some(&guard.test_hooks),
        Some(&mut children),
        Some(on_entry),
    )?;
    Ok((report, children))
}

/// The same walker for an exclusively owned disposable child of a caller's
/// directory. No workspace discovery, lock acquisition or recovery is needed.
pub(crate) fn measure_owned_tree(root: &Path, path: &Path) -> Result<WalkReport, StorageError> {
    run_owned(
        root,
        path,
        Action::Measure,
        #[cfg(feature = "storage-test-hooks")]
        None,
        None,
        None,
    )
}

pub(crate) fn remove_owned_tree(root: &Path, path: &Path) -> Result<WalkReport, StorageError> {
    run_owned(
        root,
        path,
        Action::Remove,
        #[cfg(feature = "storage-test-hooks")]
        None,
        None,
        None,
    )
}

#[cfg(feature = "storage-test-hooks")]
pub(super) fn remove_owned_tree_with_hooks(
    root: &Path,
    path: &Path,
    hooks: &super::TestHooks,
) -> Result<WalkReport, StorageError> {
    run_owned(root, path, Action::Remove, Some(hooks), None, None)
}

fn run_owned(
    root: &Path,
    path: &Path,
    action: Action,
    #[cfg(feature = "storage-test-hooks")] test_hooks: Option<&super::TestHooks>,
    child_bytes: Option<&mut BTreeMap<OsString, u64>>,
    on_entry: Option<&mut dyn FnMut(&Path)>,
) -> Result<WalkReport, StorageError> {
    let result = (|| {
        let path = super::checked_path(root, path, true)?;
        // A capability to mutate below the root never grants removal of the root.
        if path == root {
            return Err(StorageError::new(ErrorKind::InvalidPath { path }));
        }
        let parent = path.parent().expect("below root");
        super::checked_path(root, parent, false)?;
        let parent_handle = sys::open_dir(parent).map_err(|e| StorageError::io(parent, e))?;
        let name = path.file_name().expect("below root");
        let info =
            native::info(&parent_handle, name, &path).map_err(|e| StorageError::io(&path, e))?;
        let mounts = mount_points().map_err(|e| StorageError::io("/proc/self/mountinfo", e))?;
        let mut walker = Walker {
            _scope: std::marker::PhantomData,
            #[cfg(feature = "storage-test-hooks")]
            test_hooks,
            action,
            device: info.device,
            mounts,
            report: WalkReport::default(),
            child_bytes: child_bytes.map(|children| (path.as_path(), children)),
            // Coerce to the walker's shorter trait-object lifetime; `&mut`
            // inside `Option` is invariant and would not shorten by itself.
            on_entry: on_entry.map(|on_entry| on_entry as &mut dyn FnMut(&Path)),
        };
        walker.visit(parent_handle, name, &path, info);
        Ok(walker.report)
    })();
    match result {
        Err(StorageError {
            kind: ErrorKind::Io { source, .. },
            ..
        }) if action == Action::Remove && source.kind() == io::ErrorKind::NotFound => {
            Ok(WalkReport::default())
        }
        result => result,
    }
}

// Frames retain names and completion state, never directory handles. The
// current directory is the only retained traversal handle; ascend via its
// verified parent on Unix, so neither PATH_MAX nor descriptor limits grow with
// tree depth. Structural ancestors are stable for the operation (§1).
struct Frame {
    name: OsString,
    path: PathBuf,
    children: std::vec::IntoIter<OsString>,
    complete: bool,
}

impl Walker<'_> {
    fn visit(&mut self, parent: File, name: &OsStr, path: &Path, info: Info) {
        if let Some(on_entry) = &mut self.on_entry {
            on_entry(path);
        }
        let Ok(Some((mut directory, frame))) = self.enter(&parent, name, path, info, false) else {
            return;
        };
        drop(parent);
        let mut stack = vec![frame];
        'walk: while let Some(frame) = stack.last_mut() {
            if let Some(child) = frame.children.next() {
                let child_path = frame.path.join(&child);
                // Discover names even when inspecting the entry fails: a
                // governed record may be unreadable and must protect its job.
                if let Some(on_entry) = &mut self.on_entry {
                    on_entry(&child_path);
                }
                let info = match native::info(&directory, &child, &child_path) {
                    Ok(info) => info,
                    Err(error) => {
                        if self.vanished(&error) {
                            continue;
                        }
                        self.report.errors.push((child_path, error));
                        frame.complete = false;
                        continue;
                    }
                };
                match self.enter(&directory, &child, &child_path, info, true) {
                    Ok(Some((child_directory, child_frame))) => {
                        directory = child_directory;
                        stack.push(child_frame);
                    }
                    Ok(None) => {}
                    Err(()) => frame.complete = false,
                }
                continue;
            }
            let mut frame = stack.pop().expect("current frame");
            #[cfg(unix)]
            if frame.complete && self.action == Action::Sync {
                if let Err(error) = directory.sync_all() {
                    self.report.errors.push((frame.path.clone(), error));
                    frame.complete = false;
                }
            }
            let parent_path = frame.path.parent().expect("below root");
            let parent = match native::open_parent(&directory, parent_path) {
                Ok(parent) => parent,
                Err(error) => {
                    if self.vanished(&error) {
                        // The directory left was unlinked by another remover
                        // (Unix `..` of a dead directory) or its parent is gone
                        // (Windows ascends by path). Resume at the first live
                        // ancestor frame, which keeps its unvisited children.
                        while let Some(live) = stack.last() {
                            match sys::open_dir(&live.path) {
                                Ok(handle) => {
                                    directory = handle;
                                    continue 'walk;
                                }
                                Err(error) if self.vanished(&error) => {
                                    stack.pop();
                                }
                                Err(error) => {
                                    self.report.errors.push((live.path.clone(), error));
                                    return;
                                }
                            }
                        }
                        return;
                    }
                    self.report.errors.push((parent_path.into(), error));
                    return; // All unprocessed ancestors are retained.
                }
            };
            // A concurrent rename could make `..` a different directory; only
            // act through a parent whose entry still names the directory left.
            match native::is_entry(&parent, &frame.name, &directory) {
                Ok(true) => {}
                Ok(false) => {
                    self.report.errors.push((
                        frame.path.clone(),
                        io::Error::other("directory moved during walk"),
                    ));
                    return; // All unprocessed ancestors are retained.
                }
                // A concurrent remover unlinked the open directory. Ascend
                // normally so remaining siblings are still visited.
                Err(error) if self.vanished(&error) => {}
                Err(error) => {
                    self.report.errors.push((frame.path.clone(), error));
                    return;
                }
            }
            // Close the current handle before deletion (required on Windows).
            drop(directory);
            if frame.complete && self.action == Action::Remove {
                if let Err(error) = native::remove(
                    &parent,
                    &frame.name,
                    &frame.path,
                    Kind::Directory,
                    !stack.is_empty(),
                ) {
                    if !self.vanished(&error) {
                        self.report.errors.push((frame.path, error));
                        frame.complete = false;
                    }
                }
            }
            directory = parent;
            if let Some(parent_frame) = stack.last_mut() {
                parent_frame.complete &= frame.complete;
            }
        }
    }

    fn vanished(&self, error: &io::Error) -> bool {
        self.action == Action::Remove && error.kind() == io::ErrorKind::NotFound
    }

    // None means a completed leaf; Some descends into a directory. Every
    // failure is reported here and keeps the containing frames in place.
    fn enter(
        &mut self,
        parent: &File,
        name: &OsStr,
        path: &Path,
        info: Info,
        repair_parent: bool,
    ) -> Result<Option<(File, Frame)>, ()> {
        #[cfg(feature = "storage-test-hooks")]
        let device_boundary = self
            .test_hooks
            .is_some_and(|hooks| hooks.walker_device_boundary.as_deref() == Some(path));
        #[cfg(not(feature = "storage-test-hooks"))]
        let device_boundary = false;
        if info.device != self.device || device_boundary {
            self.report.skipped.push(Skipped {
                path: path.into(),
                reason: SkipReason::DeviceBoundary,
            });
            return Err(());
        }
        #[cfg(feature = "storage-test-hooks")]
        let mount_point = self
            .test_hooks
            .is_some_and(|hooks| hooks.walker_mount_point.as_deref() == Some(path));
        #[cfg(not(feature = "storage-test-hooks"))]
        let mount_point = false;
        if self.mounts.contains(path) || mount_point {
            self.report.skipped.push(Skipped {
                path: path.into(),
                reason: SkipReason::MountPoint,
            });
            return Err(());
        }
        // On Unix info/open/remove use only verified parent handles. Windows
        // info checks the entry's own reparse attributes, without rescanning
        // the entire ancestor chain for each entry.
        let result = (|| {
            if info.kind == Kind::Directory {
                let directory = native::open_directory(parent, name, path)?;
                let children = native::names(&directory, path)?.into_iter();
                return Ok(Some((
                    directory,
                    Frame {
                        name: name.into(),
                        path: path.into(),
                        children,
                        complete: true,
                    },
                )));
            }
            if info.kind == Kind::File {
                self.report.bytes = self
                    .report
                    .bytes
                    .checked_add(info.bytes)
                    .ok_or_else(|| io::Error::other("tree size overflow"))?;
                if let Some((root, children)) = &mut self.child_bytes {
                    if let Some(name) = path.strip_prefix(*root).ok().and_then(|p| p.iter().next())
                    {
                        // The total addition above bounds each child's sum too.
                        *children.entry(name.into()).or_default() += info.bytes;
                    }
                }
                if self.action == Action::Sync {
                    sys::sync_file(&native::open_file(parent, name, path)?)?;
                }
            } else if self.action == Action::Sync {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "staging contains a link or special file",
                ));
            }
            if self.action == Action::Remove {
                native::remove(parent, name, path, info.kind, repair_parent)?;
            }
            Ok(None)
        })();
        match result {
            Err(error) if self.vanished(&error) => Ok(None),
            Err(error) => {
                self.report.errors.push((path.into(), error));
                Err(())
            }
            Ok(value) => Ok(value),
        }
    }
}

#[cfg(target_os = "linux")]
fn mount_points() -> io::Result<HashSet<PathBuf>> {
    use std::os::unix::ffi::OsStringExt;
    // mountinfo escapes spaces, tabs, newlines and backslashes as octal bytes.
    let data = fs::read("/proc/self/mountinfo")?;
    let mut mounts = HashSet::new();
    for line in data.split(|b| *b == b'\n').filter(|line| !line.is_empty()) {
        let field = line.split(|b| *b == b' ').nth(4).ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidData, "invalid mountinfo record")
        })?;
        let mut decoded = Vec::new();
        let mut index = 0;
        while index < field.len() {
            if field[index] == b'\\'
                && index + 3 < field.len()
                && field[index + 1..index + 4]
                    .iter()
                    .all(|b| (b'0'..=b'7').contains(b))
            {
                let value = ((field[index + 1] - b'0') as u16 * 64)
                    + ((field[index + 2] - b'0') as u16 * 8)
                    + (field[index + 3] - b'0') as u16;
                decoded.push(value as u8);
                index += 4;
            } else {
                decoded.push(field[index]);
                index += 1;
            }
        }
        mounts.insert(PathBuf::from(OsString::from_vec(decoded)));
    }
    Ok(mounts)
}

#[cfg(not(target_os = "linux"))]
fn mount_points() -> io::Result<HashSet<PathBuf>> {
    Ok(HashSet::new())
}

#[cfg(unix)]
mod native {
    use super::*;
    use std::ffi::{CStr, CString};
    use std::os::fd::{AsRawFd, FromRawFd};
    use std::os::unix::ffi::{OsStrExt, OsStringExt};

    fn c_name(name: &OsStr) -> io::Result<CString> {
        CString::new(name.as_bytes())
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "NUL in entry"))
    }

    #[allow(clippy::unnecessary_cast)] // Native dev_t has different widths on Linux and macOS.
    pub(super) fn info(parent: &File, name: &OsStr, _path: &Path) -> io::Result<Info> {
        let name = c_name(name)?;
        let mut stat = std::mem::MaybeUninit::<libc::stat>::uninit();
        // SAFETY: parent is live, name is terminated and stat is writable. No link is followed.
        if unsafe {
            libc::fstatat(
                parent.as_raw_fd(),
                name.as_ptr(),
                stat.as_mut_ptr(),
                libc::AT_SYMLINK_NOFOLLOW,
            )
        } != 0
        {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: successful fstatat initialized the entire stat structure.
        let stat = unsafe { stat.assume_init() };
        let kind = match stat.st_mode & libc::S_IFMT {
            libc::S_IFREG => Kind::File,
            libc::S_IFDIR => Kind::Directory,
            libc::S_IFLNK => Kind::Link,
            _ => Kind::Other,
        };
        Ok(Info {
            kind,
            bytes: stat.st_size.max(0) as u64,
            device: stat.st_dev as u64,
        })
    }

    fn open(parent: &File, name: &OsStr, flags: i32) -> io::Result<File> {
        let name = c_name(name)?;
        // SAFETY: parent is live and name is terminated; O_CREAT is not set.
        let fd = unsafe {
            libc::openat(
                parent.as_raw_fd(),
                name.as_ptr(),
                flags | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            )
        };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: successful openat returned a new descriptor that this File owns.
        Ok(unsafe { File::from_raw_fd(fd) })
    }

    pub(super) fn open_directory(parent: &File, name: &OsStr, _path: &Path) -> io::Result<File> {
        open(parent, name, libc::O_RDONLY | libc::O_DIRECTORY)
    }
    pub(super) fn open_parent(directory: &File, _path: &Path) -> io::Result<File> {
        open(
            directory,
            OsStr::new(".."),
            libc::O_RDONLY | libc::O_DIRECTORY,
        )
    }
    pub(super) fn open_file(parent: &File, name: &OsStr, _path: &Path) -> io::Result<File> {
        open(parent, name, libc::O_RDONLY | libc::O_NONBLOCK)
    }

    /// Whether `parent`'s entry `name` (not followed) is the open `directory`.
    pub(super) fn is_entry(parent: &File, name: &OsStr, directory: &File) -> io::Result<bool> {
        use std::os::unix::fs::MetadataExt;
        let name = c_name(name)?;
        let mut stat = std::mem::MaybeUninit::<libc::stat>::uninit();
        // SAFETY: parent is live, name is terminated and stat is writable. No link is followed.
        if unsafe {
            libc::fstatat(
                parent.as_raw_fd(),
                name.as_ptr(),
                stat.as_mut_ptr(),
                libc::AT_SYMLINK_NOFOLLOW,
            )
        } != 0
        {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: successful fstatat initialized the entire stat structure.
        let stat = unsafe { stat.assume_init() };
        let opened = directory.metadata()?;
        #[allow(clippy::unnecessary_cast)] // Native dev_t/ino_t widths differ by platform.
        Ok((stat.st_dev as u64, stat.st_ino as u64) == (opened.dev(), opened.ino()))
    }

    struct Directory(*mut libc::DIR);
    impl Drop for Directory {
        fn drop(&mut self) {
            // SAFETY: this pointer is owned from successful fdopendir, closed exactly once.
            unsafe {
                libc::closedir(self.0);
            }
        }
    }

    pub(super) fn names(directory: &File, _path: &Path) -> io::Result<Vec<OsString>> {
        // SAFETY: directory is live; fcntl creates a separately owned close-on-exec descriptor.
        let fd = unsafe { libc::fcntl(directory.as_raw_fd(), libc::F_DUPFD_CLOEXEC, 0) };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: fd is a fresh directory descriptor; fdopendir takes ownership on success.
        let ptr = unsafe { libc::fdopendir(fd) };
        if ptr.is_null() {
            let error = io::Error::last_os_error();
            // SAFETY: fdopendir failed, so fd remains owned here.
            unsafe {
                libc::close(fd);
            }
            return Err(error);
        }
        let directory = Directory(ptr);
        let mut names = Vec::new();
        loop {
            set_errno(0);
            // SAFETY: directory is live and only this function accesses its iterator.
            let entry = unsafe { libc::readdir(directory.0) };
            if entry.is_null() {
                let error = io::Error::last_os_error();
                if error.raw_os_error() != Some(0) {
                    return Err(error);
                }
                break;
            }
            // SAFETY: readdir returned a live dirent with a terminated d_name;
            // it is copied before the next readdir call.
            let name = unsafe { CStr::from_ptr((*entry).d_name.as_ptr()) }.to_bytes();
            if name != b"." && name != b".." {
                names.push(OsString::from_vec(name.to_vec()));
            }
        }
        Ok(names)
    }

    fn set_errno(value: i32) {
        #[cfg(target_os = "linux")]
        // SAFETY: libc returns the current thread's writable errno slot.
        unsafe {
            *libc::__errno_location() = value;
        }
        #[cfg(target_os = "macos")]
        // SAFETY: libc returns the current thread's writable errno slot.
        unsafe {
            *libc::__error() = value;
        }
    }

    pub(super) fn remove(
        parent: &File,
        name: &OsStr,
        _path: &Path,
        kind: Kind,
        repair_parent: bool,
    ) -> io::Result<()> {
        use std::os::unix::fs::PermissionsExt;
        let name = c_name(name)?;
        let flags = if kind == Kind::Directory {
            libc::AT_REMOVEDIR
        } else {
            0
        };
        // SAFETY: parent is live, name is terminated; unlinkat never follows links.
        if unsafe { libc::unlinkat(parent.as_raw_fd(), name.as_ptr(), flags) } == 0 {
            return Ok(());
        }
        let error = io::Error::last_os_error();
        if error.raw_os_error() != Some(libc::EACCES) || !repair_parent {
            return Err(error);
        }
        // Only change parents inside the disposable tree, never its container.
        let mode = parent.metadata()?.permissions().mode();
        parent.set_permissions(fs::Permissions::from_mode(mode | 0o200))?;
        // SAFETY: the same live parent and terminated name are retried once.
        if unsafe { libc::unlinkat(parent.as_raw_fd(), name.as_ptr(), flags) } == 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    }
}

#[cfg(windows)]
mod native {
    use super::*;
    use std::os::windows::{
        fs::{MetadataExt, OpenOptionsExt},
        io::AsRawHandle,
    };
    use windows_sys::Win32::Storage::FileSystem::{
        BY_HANDLE_FILE_INFORMATION, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT,
        FILE_READ_ATTRIBUTES, GetFileInformationByHandle,
    };

    pub(super) fn info(_parent: &File, _name: &OsStr, path: &Path) -> io::Result<Info> {
        let metadata = fs::symlink_metadata(path)?;
        let kind = if sys::is_link(&metadata) {
            Kind::Link
        } else if metadata.is_dir() {
            Kind::Directory
        } else if metadata.is_file() {
            Kind::File
        } else {
            Kind::Other
        };
        let file = fs::OpenOptions::new()
            .access_mode(FILE_READ_ATTRIBUTES)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
            .open(path)?;
        let mut info = BY_HANDLE_FILE_INFORMATION::default();
        // SAFETY: file is live and info is writable for the synchronous call.
        if unsafe { GetFileInformationByHandle(file.as_raw_handle(), &mut info) } == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(Info {
            kind,
            bytes: metadata.file_size(),
            device: info.dwVolumeSerialNumber as u64,
        })
    }
    pub(super) fn open_directory(_parent: &File, _name: &OsStr, path: &Path) -> io::Result<File> {
        sys::open_dir(path)
    }
    pub(super) fn open_parent(_directory: &File, path: &Path) -> io::Result<File> {
        sys::open_dir(path)
    }
    // Windows traversal is path-based; each entry's reparse attributes are
    // checked when it is examined.
    pub(super) fn is_entry(_parent: &File, _name: &OsStr, _directory: &File) -> io::Result<bool> {
        Ok(true)
    }
    pub(super) fn open_file(_parent: &File, _name: &OsStr, path: &Path) -> io::Result<File> {
        fs::OpenOptions::new()
            .read(true)
            .write(true)
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
            .open(path)
    }
    pub(super) fn names(_directory: &File, path: &Path) -> io::Result<Vec<OsString>> {
        fs::read_dir(path)?
            .map(|e| e.map(|e| e.file_name()))
            .collect()
    }
    pub(super) fn remove(
        _parent: &File,
        _name: &OsStr,
        path: &Path,
        kind: Kind,
        _repair_parent: bool,
    ) -> io::Result<()> {
        use std::os::windows::ffi::OsStrExt;
        use windows_sys::Win32::{
            Foundation::ERROR_ACCESS_DENIED,
            Storage::FileSystem::{
                FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_NORMAL, FILE_ATTRIBUTE_READONLY,
                SetFileAttributesW,
            },
        };
        let metadata = fs::symlink_metadata(path)?;
        let attributes = metadata.file_attributes();
        // Directory reparse points must use RemoveDirectory, not DeleteFile.
        let remove = || {
            if kind == Kind::Directory
                || (kind == Kind::Link && attributes & FILE_ATTRIBUTE_DIRECTORY != 0)
            {
                fs::remove_dir(path)
            } else {
                fs::remove_file(path)
            }
        };
        match remove() {
            Err(error)
                if error.raw_os_error() == Some(ERROR_ACCESS_DENIED as i32)
                    && attributes & FILE_ATTRIBUTE_READONLY != 0 =>
            {
                let mut wide: Vec<_> = path.as_os_str().encode_wide().collect();
                wide.push(0);
                // SAFETY: the path is NUL-terminated. SetFileAttributes changes
                // the entry's attributes, including for a reparse point.
                let writable = attributes & !FILE_ATTRIBUTE_READONLY;
                let writable = if writable == 0 {
                    FILE_ATTRIBUTE_NORMAL
                } else {
                    writable
                };
                if unsafe { SetFileAttributesW(wide.as_ptr(), writable) } == 0 {
                    return Err(io::Error::last_os_error());
                }
                remove()
            }
            result => result,
        }
    }
}
