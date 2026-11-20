//! Filesystem observations, separated from the platform's resolution policy.

use std::io;
use std::path::Path;

/// Effective-user execute permission. Approximation is used only when Linux's
/// effective-credential syscall is unavailable, and cannot account for ACLs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Executable {
    Yes,
    No,
    Approximate(bool),
}

/// Symlink-following metadata observation; I/O errors use the Result channel.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProbeResult {
    NotFound,
    Directory,
    Other,
    Regular(Executable),
}

/// Every filesystem question made by resolution goes through this interface.
/// Implementations follow symlinks and preserve errors other than not-found.
/// Windows policy ignores the executable value on a regular file.
pub trait Probe {
    fn probe(&self, path: &Path) -> io::Result<ProbeResult>;
}

/// Native metadata and effective-credential checks; never launches a program.
#[derive(Clone, Copy, Debug, Default)]
pub struct NativeProbe;

impl Probe for NativeProbe {
    fn probe(&self, path: &Path) -> io::Result<ProbeResult> {
        let metadata = match std::fs::metadata(path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Ok(ProbeResult::NotFound);
            }
            Err(error) => return Err(error),
        };
        if metadata.is_dir() {
            return Ok(ProbeResult::Directory);
        }
        if !metadata.is_file() {
            return Ok(ProbeResult::Other);
        }
        #[cfg(unix)]
        let executable = unix::executable(path, &metadata)?;
        #[cfg(windows)]
        let executable = Executable::No; // Extension, rather than permissions, decides launchability.
        Ok(ProbeResult::Regular(executable))
    }
}

#[cfg(unix)]
mod unix {
    use std::ffi::CString;
    use std::fs::Metadata;
    use std::io;
    use std::os::unix::ffi::OsStrExt;
    #[cfg(target_os = "linux")]
    use std::os::unix::fs::MetadataExt;
    use std::path::Path;

    use super::Executable;

    pub(super) fn executable(path: &Path, metadata: &Metadata) -> io::Result<Executable> {
        let path = CString::new(path.as_os_str().as_bytes())
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?;
        // SAFETY: credential getters take no pointers and have no failure case.
        let (uid, euid, gid, egid) = unsafe {
            (
                libc::getuid(),
                libc::geteuid(),
                libc::getgid(),
                libc::getegid(),
            )
        };
        if uid == euid && gid == egid {
            // SAFETY: path is a live NUL-terminated native pathname.
            return checked(unsafe { libc::access(path.as_ptr(), libc::X_OK) });
        }
        #[cfg(target_os = "linux")]
        {
            // SAFETY: syscall arguments match faccessat2; path stays live and
            // AT_FDCWD with an absolute path requires no directory descriptor.
            let result = unsafe {
                libc::syscall(
                    libc::SYS_faccessat2,
                    libc::AT_FDCWD,
                    path.as_ptr(),
                    libc::X_OK,
                    libc::AT_EACCESS,
                )
            };
            if result == 0 {
                return Ok(Executable::Yes);
            }
            let error = io::Error::last_os_error();
            if error.raw_os_error() == Some(libc::ENOSYS) {
                return approximate(metadata, euid, egid);
            }
            denied_or_error(error)
        }
        #[cfg(not(target_os = "linux"))]
        {
            let _ = metadata;
            // SAFETY: path is a live NUL-terminated native pathname; the flags
            // select the effective credentials and follow symlinks.
            checked(unsafe {
                libc::faccessat(libc::AT_FDCWD, path.as_ptr(), libc::X_OK, libc::AT_EACCESS)
            })
        }
    }

    fn checked(result: libc::c_int) -> io::Result<Executable> {
        if result == 0 {
            Ok(Executable::Yes)
        } else {
            denied_or_error(io::Error::last_os_error())
        }
    }

    /// Only EACCES is a denial. EPERM (for example a seccomp filter refusing
    /// the syscall) is not evidence about the file and stays an error.
    fn denied_or_error(error: io::Error) -> io::Result<Executable> {
        if error.raw_os_error() == Some(libc::EACCES) {
            Ok(Executable::No)
        } else {
            Err(error)
        }
    }

    #[cfg(target_os = "linux")]
    fn approximate(
        metadata: &Metadata,
        euid: libc::uid_t,
        egid: libc::gid_t,
    ) -> io::Result<Executable> {
        let allowed = mode_allows(
            metadata.mode(),
            (metadata.uid(), metadata.gid()),
            (euid, egid),
            supplementary_groups,
        )?;
        Ok(Executable::Approximate(allowed))
    }

    /// Mode-bit execute check for effective ids. Exactly one class applies:
    /// the owner class decides for the owner even when group or other would
    /// allow, then the group class for a member of the file's group.
    #[cfg(target_os = "linux")]
    fn mode_allows(
        mode: u32,
        (owner, group): (libc::uid_t, libc::gid_t),
        (euid, egid): (libc::uid_t, libc::gid_t),
        groups: impl FnOnce() -> io::Result<Vec<libc::gid_t>>,
    ) -> io::Result<bool> {
        if euid == 0 {
            return Ok(mode & 0o111 != 0);
        }
        Ok(if euid == owner {
            mode & 0o100 != 0
        } else if egid == group || groups()?.contains(&group) {
            mode & 0o010 != 0
        } else {
            mode & 0o001 != 0
        })
    }

    #[cfg(target_os = "linux")]
    fn supplementary_groups() -> io::Result<Vec<libc::gid_t>> {
        loop {
            // SAFETY: a zero count permits a null buffer and queries the size.
            let count = unsafe { libc::getgroups(0, std::ptr::null_mut()) };
            if count < 0 {
                return Err(io::Error::last_os_error());
            }
            let mut groups: Vec<libc::gid_t> = vec![0; count as usize];
            // SAFETY: groups has room for count entries, remains live and is
            // exclusively borrowed. With count zero getgroups writes nothing.
            let actual = unsafe { libc::getgroups(count, groups.as_mut_ptr()) };
            if actual >= 0 {
                if actual as usize > groups.len() {
                    continue;
                }
                groups.truncate(actual as usize);
                return Ok(groups);
            }
            let error = io::Error::last_os_error();
            if error.raw_os_error() != Some(libc::EINVAL) {
                return Err(error);
            }
            // A concurrent credential change grew the list; query it again.
        }
    }

    #[cfg(all(test, target_os = "linux"))]
    mod tests {
        use super::mode_allows;

        #[test]
        fn mode_fallback_uses_exactly_one_class() {
            let none = || -> std::io::Result<Vec<libc::gid_t>> { Ok(Vec::new()) };
            let member = || -> std::io::Result<Vec<libc::gid_t>> { Ok(vec![20]) };
            // Owner without u+x is refused although group and other allow.
            assert!(!mode_allows(0o011, (1, 2), (1, 9), none).unwrap());
            assert!(mode_allows(0o100, (1, 2), (1, 9), none).unwrap());
            // Group class, by effective or supplementary group, before other.
            assert!(!mode_allows(0o101, (1, 2), (5, 2), none).unwrap());
            assert!(mode_allows(0o010, (1, 20), (5, 9), member).unwrap());
            assert!(!mode_allows(0o001, (1, 20), (5, 9), member).unwrap());
            assert!(mode_allows(0o001, (1, 2), (5, 9), member).unwrap());
            // Root needs any execute bit.
            assert!(mode_allows(0o001, (1, 2), (0, 0), none).unwrap());
            assert!(!mode_allows(0o644, (1, 2), (0, 0), none).unwrap());
        }
    }
}
