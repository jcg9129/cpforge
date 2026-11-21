use super::super::{InstalledLimit, NotStarted, OutputSpec, ProgramSpec, StdinSpec, Termination};
use super::RawRusage;
use std::fs::File;
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::process::CommandExt;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};
use tokio::io::unix::AsyncFd;

#[path = "linux_proc.rs"]
mod proc;
pub(in crate::supervisor) use proc::CleanupScan;

pub(in crate::supervisor) type Observer = AsyncFd<OwnedFd>;

pub(in crate::supervisor) fn spawn(
    spec: ProgramSpec,
    limits: &[InstalledLimit],
    fail_limit_install: bool,
) -> Result<(i32, Instant), NotStarted> {
    let stdin = match spec.stdin {
        StdinSpec::Null => Stdio::null(),
        StdinSpec::Pipe(end) => end.into(),
        StdinSpec::Connected => return Err(NotStarted::Unsupported),
        StdinSpec::File(path) => File::open(path)
            .map(Stdio::from)
            .map_err(NotStarted::Setup)?,
    };
    fn output(spec: OutputSpec) -> Stdio {
        match spec {
            OutputSpec::Null => Stdio::null(),
            OutputSpec::Connected => unreachable!("connected endpoints resolved by session"),
            OutputSpec::Pipe(end) => end.into_stdio(),
            OutputSpec::Capture(_) => unreachable!("capture prepared before creation"),
        }
    }
    let mut command = Command::new(spec.program);
    command
        .args(spec.args)
        .current_dir(spec.cwd)
        .env_clear()
        .envs(spec.env)
        .process_group(0)
        .stdin(stdin)
        .stdout(output(spec.stdout))
        .stderr(output(spec.stderr));
    let phase = super::limits::install_on_spawn(&mut command, limits, fail_limit_install)
        .map_err(NotStarted::Setup)?;
    let result = command.spawn();
    let mut child = result.map_err(|error| {
        if let Some(errno) = phase.as_ref().and_then(|pipe| pipe.read_failure()) {
            NotStarted::Limit(errno)
        } else {
            NotStarted::CreateFailed {
                api: "spawn",
                raw_error: error.raw_os_error().unwrap_or(0),
            }
        }
    })?;
    let create_end = Instant::now();
    let pid = child.id() as i32;
    // No Stdio::piped() is used, but explicitly move any handles before forgetting
    // the Child: all consuming waits belong to this module, never std's Child.
    drop(child.stdin.take());
    drop(child.stdout.take());
    drop(child.stderr.take());
    std::mem::forget(child);
    Ok((pid, create_end))
}

pub(in crate::supervisor) fn observer(pid: i32) -> io::Result<Observer> {
    // SAFETY: pid is our owned, unreaped root; pidfd_open takes integer arguments
    // and returns a new CLOEXEC fd on success. It does not consume child status.
    let fd = unsafe { libc::syscall(libc::SYS_pidfd_open, pid, 0) };
    if fd == -1 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: successful pidfd_open returned a uniquely owned descriptor.
    let fd = unsafe { OwnedFd::from_raw_fd(fd as i32) };
    // SAFETY: OwnedFd keeps this descriptor valid and unchanged until the
    // AsyncFd drops; it is never extracted, replaced, or closed behind its back.
    Ok(unsafe { AsyncFd::register_with_interest(fd, tokio::io::Interest::READABLE) }?)
}

pub(in crate::supervisor) fn observe(pid: i32) -> io::Result<bool> {
    observe_status(pid).map(|status| status.is_some())
}

pub(in crate::supervisor) fn observe_status(pid: i32) -> io::Result<Option<Termination>> {
    loop {
        // SAFETY: zero is a valid initial representation of siginfo_t; waitid
        // writes it, and the WNOHANG no-event case leaves si_pid zero.
        let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
        // SAFETY: positive pid selects exactly our root; info is writable, and
        // WNOWAIT retains the zombie and its numeric identity protection.
        let result = unsafe {
            libc::waitid(
                libc::P_PID,
                pid as u32,
                &mut info,
                libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
            )
        };
        if result == 0 {
            // SAFETY: waitid initialized the siginfo union for this operation.
            if unsafe { info.si_pid() } == 0 {
                return Ok(None);
            }
            // SAFETY: waitid initialized the exited-child siginfo union.
            let status = unsafe { info.si_status() };
            return Ok(Some(match info.si_code {
                libc::CLD_EXITED => Termination::Exited {
                    code: i64::from(status),
                },
                libc::CLD_KILLED | libc::CLD_DUMPED => Termination::Signalled {
                    signal: status,
                    core_dumped: info.si_code == libc::CLD_DUMPED,
                },
                _ => Termination::Unknown,
            }));
        }
        let error = io::Error::last_os_error();
        if error.raw_os_error() != Some(libc::EINTR) {
            return Err(error);
        }
    }
}

/// Nonblocking check of the observer actually installed for this run.
pub(in crate::supervisor) fn exit_check(observer: Option<&Observer>, pid: i32) -> io::Result<bool> {
    let Some(observer) = observer else {
        return observe(pid);
    };
    let mut fd = libc::pollfd {
        fd: observer.get_ref().as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    };
    loop {
        // SAFETY: fd points to one initialized pollfd; the observer owns its fd.
        let result = unsafe { libc::poll(&mut fd, 1, 0) };
        if result >= 0 {
            if fd.revents & (libc::POLLERR | libc::POLLNVAL) != 0 {
                return Err(io::Error::other("pidfd poll failed"));
            }
            return Ok(fd.revents & libc::POLLIN != 0);
        }
        let error = io::Error::last_os_error();
        if error.kind() != io::ErrorKind::Interrupted {
            return Err(error);
        }
    }
}

pub(in crate::supervisor) fn cleanup_scan(
    observer: Option<&Observer>,
) -> Result<CleanupScan, super::super::UnverifiedReason> {
    CleanupScan::new(
        std::path::Path::new("/proc"),
        observer.map(|o| o.get_ref().as_raw_fd()),
    )
}

pub(in crate::supervisor) fn kill_group(pgid: i32) -> io::Result<()> {
    // SAFETY: caller holds the live-run registry lock and checked capability;
    // the unreaped root pins this positive process-group id against reuse.
    if unsafe { libc::killpg(pgid, libc::SIGKILL) } == 0 {
        return Ok(());
    }
    let error = io::Error::last_os_error();
    if error.raw_os_error() == Some(libc::ESRCH) {
        Ok(())
    } else {
        Err(error)
    }
}

pub(in crate::supervisor) fn reap(pid: i32) -> io::Result<Option<(Termination, RawRusage)>> {
    loop {
        let mut status = 0;
        // SAFETY: zero is valid for rusage; wait4 writes the whole output.
        let mut usage: libc::rusage = unsafe { std::mem::zeroed() };
        // SAFETY: positive pid waits only for the owned root. Both pointers are
        // writable; WNOHANG never blocks. This is the sole consuming wait.
        let result = unsafe { libc::wait4(pid, &mut status, libc::WNOHANG, &mut usage) };
        if result == 0 {
            return Ok(None);
        }
        if result == -1 {
            let error = io::Error::last_os_error();
            if error.raw_os_error() == Some(libc::EINTR) {
                continue;
            }
            return Err(error);
        }
        let termination = if libc::WIFEXITED(status) {
            Termination::Exited {
                code: i64::from(libc::WEXITSTATUS(status)),
            }
        } else if libc::WIFSIGNALED(status) {
            Termination::Signalled {
                signal: libc::WTERMSIG(status),
                core_dumped: libc::WCOREDUMP(status),
            }
        } else {
            Termination::Unknown
        };
        fn duration(time: libc::timeval) -> Duration {
            Duration::from_secs(time.tv_sec.max(0) as u64)
                + Duration::from_micros(time.tv_usec.max(0) as u64)
        }
        return Ok(Some((
            termination,
            RawRusage {
                maxrss_bytes: (usage.ru_maxrss.max(0) as u64).saturating_mul(1024),
                user: duration(usage.ru_utime),
                system: duration(usage.ru_stime),
            },
        )));
    }
}

pub(in crate::supervisor) fn init_process() -> io::Result<()> {
    // SAFETY: close_range with CLOEXEC only sets descriptor flags; no memory
    // pointers or descriptor ownership are transferred.
    if unsafe {
        libc::syscall(
            libc::SYS_close_range,
            3_u32,
            u32::MAX,
            libc::CLOSE_RANGE_CLOEXEC,
        )
    } == 0
    {
        return Ok(());
    }
    let error = io::Error::last_os_error();
    if !matches!(error.raw_os_error(), Some(libc::ENOSYS | libc::EINVAL)) {
        return Err(error);
    }
    cloexec_from_proc()
}

fn cloexec_from_proc() -> io::Result<()> {
    // Own the directory stream explicitly so its descriptor can be excluded.
    // SAFETY: the constant is a valid NUL-terminated path.
    let directory = unsafe { libc::opendir(c"/proc/self/fd".as_ptr()) };
    if directory.is_null() {
        return Err(io::Error::last_os_error());
    }
    struct Directory(*mut libc::DIR);
    impl Drop for Directory {
        fn drop(&mut self) {
            // SAFETY: this guard uniquely owns the directory stream.
            unsafe {
                libc::closedir(self.0);
            }
        }
    }
    let directory = Directory(directory);
    // SAFETY: directory remains open until its guard drops.
    let own_fd = unsafe { libc::dirfd(directory.0) };
    loop {
        // SAFETY: errno is thread-local; resetting it distinguishes EOF/error.
        unsafe {
            *libc::__errno_location() = 0;
        }
        // SAFETY: directory is live; each entry is examined before the next call.
        let entry = unsafe { libc::readdir(directory.0) };
        if entry.is_null() {
            let error = io::Error::last_os_error();
            return if error.raw_os_error() == Some(0) {
                Ok(())
            } else {
                Err(error)
            };
        }
        // SAFETY: readdir returned a live entry with a NUL-terminated d_name.
        let name = unsafe { std::ffi::CStr::from_ptr((*entry).d_name.as_ptr()) };
        let Some(fd) = name.to_str().ok().and_then(|name| name.parse::<i32>().ok()) else {
            continue;
        };
        if fd < 3 || fd == own_fd {
            continue;
        }
        // SAFETY: this only changes flags on the enumerated descriptor. Another
        // thread may close it; EBADF is harmless during startup enumeration.
        if unsafe { libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) } == -1 {
            let error = io::Error::last_os_error();
            if error.raw_os_error() != Some(libc::EBADF) {
                return Err(error);
            }
        }
    }
}

/// Stop a session root while preserving descendants for the final transport drain.
pub(in crate::supervisor) fn kill_root(pid: i32) -> io::Result<()> {
    // SAFETY: the caller owns the unreaped root and its signalling capability.
    if unsafe { libc::kill(pid, libc::SIGKILL) } == 0 {
        return Ok(());
    }
    let error = io::Error::last_os_error();
    if error.raw_os_error() == Some(libc::ESRCH) {
        Ok(())
    } else {
        Err(error)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::fd::AsRawFd;

    #[test]
    fn init_preserves_sigchld_default_and_marks_inherited_fd() {
        let file = File::open("/dev/null").unwrap();
        // SAFETY: file is live; this deliberately models an inherited fd.
        assert_eq!(
            unsafe { libc::fcntl(file.as_raw_fd(), libc::F_SETFD, 0) },
            0
        );
        super::super::super::init_process().unwrap();
        // SAFETY: file remains owned and live; F_GETFD only reads flags.
        assert_ne!(
            unsafe { libc::fcntl(file.as_raw_fd(), libc::F_GETFD) } & libc::FD_CLOEXEC,
            0
        );
        // SAFETY: zero is valid initial storage; sigaction with null act only
        // reads the disposition and never changes SIGCHLD handling.
        let mut action: libc::sigaction = unsafe { std::mem::zeroed() };
        // SAFETY: action is writable and no new disposition is supplied.
        assert_eq!(
            unsafe { libc::sigaction(libc::SIGCHLD, std::ptr::null(), &mut action) },
            0
        );
        assert_eq!(action.sa_sigaction, libc::SIG_DFL);
        assert_eq!(action.sa_flags & libc::SA_NOCLDWAIT, 0);
    }

    #[test]
    fn proc_fallback_marks_inherited_fd() {
        super::super::super::init_process().unwrap();
        let file = File::open("/dev/null").unwrap();
        // SAFETY: file is live; only its descriptor flags are changed/read.
        assert_eq!(
            unsafe { libc::fcntl(file.as_raw_fd(), libc::F_SETFD, 0) },
            0
        );
        cloexec_from_proc().unwrap();
        // SAFETY: file is still live; F_GETFD reads its descriptor flags.
        assert_ne!(
            unsafe { libc::fcntl(file.as_raw_fd(), libc::F_GETFD) } & libc::FD_CLOEXEC,
            0
        );
    }
}
