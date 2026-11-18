//! Linux admission sampling and allocation-free child limit installation.
use crate::supervisor::{
    InstalledLimit, LimitValidation, LocalLimit, NotStarted, OuterRestriction,
};
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::process::CommandExt;
use std::process::Command;

fn inherited() -> io::Result<libc::rlimit> {
    let mut limit = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    // SAFETY: limit is a writable rlimit and RLIMIT_AS is supported on Linux.
    if unsafe { libc::getrlimit(libc::RLIMIT_AS, &mut limit) } == -1 {
        Err(io::Error::last_os_error())
    } else {
        Ok(limit)
    }
}

pub(in crate::supervisor) fn prepare_limits(
    limits: &[LocalLimit],
) -> Result<Vec<InstalledLimit>, NotStarted> {
    use LimitValidation::{Duplicate, OutOfRange, Zero};
    let mut bytes = None;
    for limit in limits {
        match *limit {
            LocalLimit::JobCommit { .. } => return Err(NotStarted::UnsupportedLimit),
            LocalLimit::AddressSpace { bytes: value } => {
                if bytes.replace(value).is_some() {
                    return Err(NotStarted::InvalidLimit(Duplicate));
                }
            }
        }
    }
    let Some(bytes) = bytes else {
        return Ok(Vec::new());
    };
    if bytes == 0 {
        return Err(NotStarted::InvalidLimit(Zero));
    }
    // SAFETY: sysconf has no pointer arguments and does not mutate limits.
    let page = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
    let page = u64::try_from(page)
        .ok()
        .filter(|page| *page > 0)
        .ok_or_else(|| NotStarted::Setup(io::Error::other("invalid page size")))?;
    let normalised = bytes
        .checked_add(page - 1)
        .map(|value| value / page * page)
        // On Linux RLIM_INFINITY is also the maximum representable rlim_t.
        .filter(|value| *value < libc::RLIM_INFINITY)
        .ok_or(NotStarted::InvalidLimit(OutOfRange))?;
    let inherited =
        inherited().map_err(|error| NotStarted::Limit(error.raw_os_error().unwrap_or(0)))?;
    Ok(vec![InstalledLimit {
        configured: LocalLimit::AddressSpace { bytes },
        inherited: Some((inherited.rlim_cur, inherited.rlim_max)),
        normalised,
        installed: Some(normalised.min(inherited.rlim_max)),
        mechanism: "setrlimit RLIMIT_AS",
        outer: OuterRestriction::None,
    }])
}

const LIMIT_PHASE: u8 = 1;

#[derive(Debug)]
pub(super) struct PhasePipe {
    read: OwnedFd,
    // Keep the write descriptor alive through spawn; the closure borrows its
    // raw number. CLOEXEC closes the child's copy after successful exec.
    _write: OwnedFd,
}

impl PhasePipe {
    pub(super) fn read_failure(&self) -> Option<i32> {
        let mut record = [0_u8; 5];
        loop {
            // SAFETY: we own the nonblocking read end and record is writable.
            let count = unsafe {
                libc::read(
                    self.read.as_raw_fd(),
                    record.as_mut_ptr().cast(),
                    record.len(),
                )
            };
            if count == record.len() as isize && record[0] == LIMIT_PHASE {
                return Some(i32::from_ne_bytes(record[1..].try_into().unwrap()));
            }
            if count != -1 || io::Error::last_os_error().raw_os_error() != Some(libc::EINTR) {
                return None;
            }
        }
    }
}

pub(super) fn install_on_spawn(
    command: &mut Command,
    limits: &[InstalledLimit],
    fail: bool,
) -> io::Result<Option<PhasePipe>> {
    let Some(limit) = limits.first() else {
        return Ok(None);
    };
    let value = limit.installed.expect("admission prepared limit");
    let limit = if fail {
        // Test-only invalid soft > hard forces the real syscall to fail EINVAL.
        libc::rlimit {
            rlim_cur: 1,
            rlim_max: 0,
        }
    } else {
        libc::rlimit {
            rlim_cur: value,
            rlim_max: value,
        }
    };
    let mut fds = [-1; 2];
    // SAFETY: fds is writable for both returned descriptors. Both are CLOEXEC;
    // nonblocking read ensures a spawn error can never hang the parent.
    if unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC | libc::O_NONBLOCK) } == -1 {
        return Err(io::Error::last_os_error());
    }
    let pipe = PhasePipe {
        // SAFETY: successful pipe2 returned two uniquely owned descriptors.
        read: unsafe { OwnedFd::from_raw_fd(fds[0]) },
        // SAFETY: the second descriptor is distinct and uniquely owned.
        _write: unsafe { OwnedFd::from_raw_fd(fds[1]) },
    };
    let write = pipe._write.as_raw_fd();
    // SAFETY: all data and descriptors are prepared before fork. The closure
    // calls only setrlimit, errno access and write, with stack-only records and
    // io::Error's allocation-free raw OS error representation. It takes no locks.
    unsafe {
        command.pre_exec(move || {
            if libc::setrlimit(libc::RLIMIT_AS, &limit) == 0 {
                return Ok(());
            }
            let error = io::Error::last_os_error();
            let errno = error.raw_os_error().unwrap_or(libc::EINVAL);
            let mut record = [LIMIT_PHASE, 0, 0, 0, 0];
            record[1..].copy_from_slice(&errno.to_ne_bytes());
            // One atomic record, smaller than PIPE_BUF, into this child's private pipe.
            loop {
                if libc::write(write, record.as_ptr().cast(), record.len()) != -1
                    || io::Error::last_os_error().raw_os_error() != Some(libc::EINTR)
                {
                    break;
                }
            }
            Err(error)
        });
    }
    Ok(Some(pipe))
}

type Probe = Result<(), String>;

pub(in crate::supervisor) fn probe_capabilities() -> (Probe, Probe, Probe, Probe) {
    // SAFETY: rusage is initialized by getrusage and remains local to this call.
    let mut usage: libc::rusage = unsafe { std::mem::zeroed() };
    // SAFETY: usage is writable and RUSAGE_SELF does not consume child status.
    let accounting = if unsafe { libc::getrusage(libc::RUSAGE_SELF, &mut usage) } == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error().to_string())
    };
    let address_space = inherited().map(|_| ()).map_err(|e| e.to_string());
    // SAFETY: getpid and pidfd_open take no pointers, and consume no child state.
    let fd = unsafe { libc::syscall(libc::SYS_pidfd_open, libc::getpid(), 0) };
    if fd == -1 {
        let error = io::Error::last_os_error().to_string();
        return (accounting, address_space, Err(error.clone()), Err(error));
    }
    // SAFETY: pidfd_open returned a uniquely owned CLOEXEC descriptor.
    let fd = unsafe { OwnedFd::from_raw_fd(fd as i32) };
    let procfs = super::CleanupScan::new(std::path::Path::new("/proc"), Some(fd.as_raw_fd()))
        .map(|_| ())
        .map_err(|reason| format!("{reason:?}"));
    (accounting, address_space, Ok(()), procfs)
}
