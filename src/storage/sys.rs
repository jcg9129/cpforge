//! Small platform boundary. No platform operation uses a copy-on-rename flag.

use std::fs::{self, File};
use std::io;
use std::path::Path;

#[cfg(unix)]
use std::os::fd::AsRawFd;

pub(crate) fn is_link(metadata: &fs::Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        metadata.file_attributes()
            & windows_sys::Win32::Storage::FileSystem::FILE_ATTRIBUTE_REPARSE_POINT
            != 0
    }
    #[cfg(unix)]
    {
        metadata.file_type().is_symlink()
    }
}

pub(super) fn name_too_long(error: &io::Error, _path: &Path) -> bool {
    #[cfg(unix)]
    {
        error.raw_os_error() == Some(libc::ENAMETOOLONG)
    }
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        use windows_sys::Win32::Foundation::{ERROR_FILENAME_EXCED_RANGE, ERROR_INVALID_NAME};
        matches!(error.raw_os_error(), Some(code)
            if code == ERROR_FILENAME_EXCED_RANGE as i32 || code == ERROR_INVALID_NAME as i32)
            && _path.components().any(|component| {
                matches!(component, std::path::Component::Normal(name)
                    if name.encode_wide().count() > 255)
            })
    }
}

pub(super) fn cross_device(error: &io::Error) -> bool {
    #[cfg(unix)]
    {
        error.raw_os_error() == Some(libc::EXDEV)
    }
    #[cfg(windows)]
    {
        error.raw_os_error() == Some(windows_sys::Win32::Foundation::ERROR_NOT_SAME_DEVICE as i32)
    }
}

pub(super) fn flag_unsupported(error: &io::Error) -> bool {
    #[cfg(unix)]
    {
        error.kind() == io::ErrorKind::Unsupported
            || matches!(
                error.raw_os_error(),
                Some(code) if code == libc::EINVAL || code == libc::ENOSYS
                    || code == libc::EOPNOTSUPP || code == libc::ENOTSUP
            )
    }
    #[cfg(windows)]
    {
        use windows_sys::Win32::Foundation::{ERROR_INVALID_FUNCTION, ERROR_NOT_SUPPORTED};
        matches!(error.raw_os_error(), Some(code) if code == ERROR_INVALID_FUNCTION as i32 || code == ERROR_NOT_SUPPORTED as i32)
    }
}

pub(super) fn sync_file(file: &File) -> io::Result<()> {
    // std uses FlushFileBuffers on Windows and F_FULLFSYNC (with an fsync
    // fallback) on macOS, including for the directory handles synced below.
    file.sync_all()
}

pub(super) fn sync_dir(path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        open_dir(path)?.sync_all()
    }
    #[cfg(windows)]
    {
        let _ = path;
        Ok(())
    }
}

pub(super) fn open_dir(path: &Path) -> io::Result<File> {
    let mut options = fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_DIRECTORY | libc::O_CLOEXEC);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        use windows_sys::Win32::Storage::FileSystem::{
            FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT,
        };
        options.custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT);
    }
    options.open(path)
}

/// Compare the locked handle with the current directory entry without following links.
pub(super) fn same_file(file: &File, path: &Path) -> io::Result<bool> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error),
    };
    if is_link(&metadata) {
        return Ok(false);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let opened = file.metadata()?;
        Ok((opened.dev(), opened.ino()) == (metadata.dev(), metadata.ino()))
    }
    #[cfg(windows)]
    {
        use std::os::windows::{fs::OpenOptionsExt, io::AsRawHandle};
        use windows_sys::Win32::Storage::FileSystem::{
            BY_HANDLE_FILE_INFORMATION, FILE_FLAG_OPEN_REPARSE_POINT, FILE_READ_ATTRIBUTES,
            GetFileInformationByHandle,
        };
        let current = match fs::OpenOptions::new()
            .access_mode(FILE_READ_ATTRIBUTES)
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
            .open(path)
        {
            Ok(current) => current,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
            Err(error) => return Err(error),
        };
        let mut opened = BY_HANDLE_FILE_INFORMATION::default();
        let mut entry = BY_HANDLE_FILE_INFORMATION::default();
        // SAFETY: both handles are live and the output structures are writable.
        if unsafe { GetFileInformationByHandle(file.as_raw_handle(), &mut opened) } == 0
            || unsafe { GetFileInformationByHandle(current.as_raw_handle(), &mut entry) } == 0
        {
            return Err(io::Error::last_os_error());
        }
        Ok((
            opened.dwVolumeSerialNumber,
            opened.nFileIndexHigh,
            opened.nFileIndexLow,
        ) == (
            entry.dwVolumeSerialNumber,
            entry.nFileIndexHigh,
            entry.nFileIndexLow,
        ))
    }
}

pub(super) fn try_lock(file: &File) -> io::Result<bool> {
    #[cfg(unix)]
    {
        // SAFETY: the descriptor is live and flock has no pointer arguments.
        if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
            return Ok(true);
        }
        let error = io::Error::last_os_error();
        if matches!(error.raw_os_error(), Some(libc::EWOULDBLOCK | libc::EINTR)) {
            Ok(false)
        } else {
            Err(error)
        }
    }
    #[cfg(windows)]
    {
        use std::os::windows::io::AsRawHandle;
        use windows_sys::Win32::{
            Foundation::ERROR_LOCK_VIOLATION,
            Storage::FileSystem::{LOCKFILE_EXCLUSIVE_LOCK, LOCKFILE_FAIL_IMMEDIATELY, LockFileEx},
            System::IO::OVERLAPPED,
        };
        let mut overlapped = OVERLAPPED::default();
        // SAFETY: file is live, overlapped is valid for this synchronous call;
        // offsets are zero and the mandatory range is exactly [0, 1).
        if unsafe {
            LockFileEx(
                file.as_raw_handle(),
                LOCKFILE_EXCLUSIVE_LOCK | LOCKFILE_FAIL_IMMEDIATELY,
                0,
                1,
                0,
                &mut overlapped,
            )
        } != 0
        {
            return Ok(true);
        }
        let error = io::Error::last_os_error();
        if error.raw_os_error() == Some(ERROR_LOCK_VIOLATION as i32) {
            Ok(false)
        } else {
            Err(error)
        }
    }
}

pub(super) fn unlock(file: &File) {
    #[cfg(unix)]
    {
        // SAFETY: file remains live until after unlock.
        let _ = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_UN) };
    }
    #[cfg(windows)]
    {
        use std::os::windows::io::AsRawHandle;
        use windows_sys::Win32::{Storage::FileSystem::UnlockFileEx, System::IO::OVERLAPPED};
        let mut overlapped = OVERLAPPED::default();
        // SAFETY: the same live file and range that were locked are supplied.
        let _ = unsafe { UnlockFileEx(file.as_raw_handle(), 0, 1, 0, &mut overlapped) };
    }
}

#[cfg(unix)]
pub(super) fn c_path(path: &Path) -> io::Result<std::ffi::CString> {
    use std::os::unix::ffi::OsStrExt;
    std::ffi::CString::new(path.as_os_str().as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "NUL in path"))
}

#[cfg(windows)]
fn wide_path(path: &Path) -> io::Result<Vec<u16>> {
    use std::os::windows::ffi::OsStrExt;
    // canonicalize the existing parent through std to obtain a verbatim path,
    // including for a destination that does not exist yet.
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "missing parent"))?;
    let name = path
        .file_name()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "missing name"))?;
    let absolute = fs::canonicalize(parent)?.join(name);
    let mut wide: Vec<u16> = absolute.as_os_str().encode_wide().collect();
    if wide.contains(&0) {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, "NUL in path"));
    }
    wide.push(0);
    Ok(wide)
}

pub(super) fn replace(source: &Path, target: &Path) -> io::Result<()> {
    // Unix `rename`. On Windows, std renames through the source handle with
    // `FileRenameInfoEx` and `FILE_RENAME_FLAG_REPLACE_IF_EXISTS |
    // FILE_RENAME_FLAG_POSIX_SEMANTICS` (falling back to `FileRenameInfo`
    // before Windows 10 1607), so a destination held open with delete sharing
    // is replaced while existing readers keep the old file. It never copies.
    fs::rename(source, target)
}

pub(super) fn rename_exclusive(source: &Path, target: &Path) -> io::Result<()> {
    #[cfg(target_os = "linux")]
    {
        let source = c_path(source)?;
        let target = c_path(target)?;
        // SAFETY: both C strings are terminated and live for the syscall.
        let result = unsafe {
            libc::syscall(
                libc::SYS_renameat2,
                libc::AT_FDCWD,
                source.as_ptr(),
                libc::AT_FDCWD,
                target.as_ptr(),
                libc::RENAME_NOREPLACE,
            )
        };
        if result == 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    }
    #[cfg(target_os = "macos")]
    {
        let source = c_path(source)?;
        let target = c_path(target)?;
        // SAFETY: both C strings are terminated and live for the call.
        if unsafe { libc::renamex_np(source.as_ptr(), target.as_ptr(), libc::RENAME_EXCL) } == 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    }
    #[cfg(windows)]
    {
        move_file(source, target, 0)
    }
    #[cfg(all(unix, not(any(target_os = "linux", target_os = "macos"))))]
    {
        let _ = (source, target);
        Err(io::Error::from_raw_os_error(libc::ENOSYS))
    }
}

#[cfg(windows)]
fn move_file(source: &Path, target: &Path, flags: u32) -> io::Result<()> {
    let source = wide_path(source)?;
    let target = wide_path(target)?;
    // SAFETY: both buffers contain valid NUL-terminated paths and remain live.
    if unsafe {
        windows_sys::Win32::Storage::FileSystem::MoveFileExW(
            source.as_ptr(),
            target.as_ptr(),
            flags,
        )
    } != 0
    {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

#[cfg(windows)]
pub(super) fn retryable_publication(error: &io::Error) -> bool {
    use windows_sys::Win32::Foundation::{ERROR_ACCESS_DENIED, ERROR_SHARING_VIOLATION};
    matches!(error.raw_os_error(), Some(code) if code == ERROR_ACCESS_DENIED as i32 || code == ERROR_SHARING_VIOLATION as i32)
}

// MoveFileExW reports an existing directory as ERROR_ACCESS_DENIED.
pub(super) fn exclusive_access_denied(error: &io::Error) -> bool {
    #[cfg(windows)]
    {
        error.raw_os_error() == Some(windows_sys::Win32::Foundation::ERROR_ACCESS_DENIED as i32)
    }
    #[cfg(unix)]
    {
        let _ = error;
        false
    }
}

/// Native filesystem identity for an unsupported-operation diagnostic.
pub(super) fn filesystem(path: &Path) -> String {
    filesystem_name(path).unwrap_or_else(|error| format!("unknown at {} ({error})", path.display()))
}

#[cfg(target_os = "linux")]
fn filesystem_name(path: &Path) -> io::Result<String> {
    let path = c_path(path)?;
    let mut stat = std::mem::MaybeUninit::<libc::statfs>::uninit();
    // SAFETY: path is terminated and stat is valid writable storage.
    if unsafe { libc::statfs(path.as_ptr(), stat.as_mut_ptr()) } != 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: successful statfs initialized stat.
    let stat = unsafe { stat.assume_init() };
    Ok(match stat.f_type {
        0xef53 => "ext4".into(),
        0x58465342 => "xfs".into(),
        0x9123683e => "btrfs".into(),
        0x01021994 => "tmpfs".into(),
        other => format!("filesystem type {other:#x}"),
    })
}

#[cfg(target_os = "macos")]
fn filesystem_name(path: &Path) -> io::Result<String> {
    let path = c_path(path)?;
    let mut stat = std::mem::MaybeUninit::<libc::statfs>::uninit();
    // SAFETY: path is terminated and stat is valid writable storage.
    if unsafe { libc::statfs(path.as_ptr(), stat.as_mut_ptr()) } != 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: successful statfs initialized stat, including terminated f_fstypename.
    let stat = unsafe { stat.assume_init() };
    // SAFETY: statfs provides a NUL-terminated filesystem name in f_fstypename.
    Ok(
        unsafe { std::ffi::CStr::from_ptr(stat.f_fstypename.as_ptr()) }
            .to_string_lossy()
            .into_owned(),
    )
}

#[cfg(windows)]
fn filesystem_name(path: &Path) -> io::Result<String> {
    use std::os::windows::io::AsRawHandle;
    let file = open_dir(path)?;
    let mut name = [0_u16; 64];
    // SAFETY: file is live, optional outputs are null, and name has the stated capacity.
    if unsafe {
        windows_sys::Win32::Storage::FileSystem::GetVolumeInformationByHandleW(
            file.as_raw_handle(),
            std::ptr::null_mut(),
            0,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            name.as_mut_ptr(),
            name.len() as u32,
        )
    } == 0
    {
        return Err(io::Error::last_os_error());
    }
    Ok(String::from_utf16_lossy(
        &name[..name.iter().position(|c| *c == 0).unwrap_or(name.len())],
    ))
}

#[cfg(all(unix, not(any(target_os = "linux", target_os = "macos"))))]
fn filesystem_name(path: &Path) -> io::Result<String> {
    Ok(format!("unknown at {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn unsupported_rename_flags_include_enotsup() {
        for code in [libc::ENOTSUP, libc::EOPNOTSUPP, libc::EINVAL, libc::ENOSYS] {
            assert!(flag_unsupported(&io::Error::from_raw_os_error(code)));
        }
        assert!(!flag_unsupported(&io::Error::from_raw_os_error(
            libc::EACCES
        )));
    }

    #[cfg(windows)]
    #[test]
    fn invalid_name_is_length_error_only_for_overlong_components() {
        use windows_sys::Win32::Foundation::{ERROR_FILENAME_EXCED_RANGE, ERROR_INVALID_NAME};
        for code in [ERROR_INVALID_NAME, ERROR_FILENAME_EXCED_RANGE] {
            let error = io::Error::from_raw_os_error(code as i32);
            for name in ["x".repeat(256), "😀".repeat(128)] {
                assert!(name_too_long(
                    &error,
                    &Path::new(r"C:\workspace").join(name)
                ));
            }
            for name in ["x".repeat(255), "bad:name".into(), "😀".repeat(127)] {
                assert!(!name_too_long(
                    &error,
                    &Path::new(r"C:\workspace").join(name)
                ));
            }
            let long_total = Path::new(r"C:\workspace").join("short/".repeat(60));
            assert!(!name_too_long(&error, &long_total));
        }
        assert!(!name_too_long(
            &io::Error::from_raw_os_error(
                windows_sys::Win32::Foundation::ERROR_ACCESS_DENIED as i32
            ),
            Path::new(&"x".repeat(256)),
        ));
    }
}
