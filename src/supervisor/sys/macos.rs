//! Darwin native operations. No application code runs between fork and exec.
use super::super::{
    InstalledLimit, LocalLimit, NotStarted, OutputSpec, ProgramSpec, StdinSpec, Termination,
};
use super::RawRusage;
use std::fs::File;
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::process::CommandExt;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, Instant};
use tokio::io::unix::AsyncFd;

// Darwin has no pipe2 or kqueue1. Serialize create -> FD_CLOEXEC with spawn.
// Never acquire this lock in a pre_exec callback. Files use atomic O_CLOEXEC.
static CREATION: Mutex<()> = Mutex::new(());
pub(in crate::supervisor) fn creation_lock() -> MutexGuard<'static, ()> {
    CREATION.lock().unwrap_or_else(|error| error.into_inner())
}

pub(in crate::supervisor) fn prepare_limits(
    limits: &[LocalLimit],
) -> Result<Vec<InstalledLimit>, NotStarted> {
    if limits.is_empty() {
        Ok(Vec::new())
    } else {
        Err(NotStarted::UnsupportedLimit)
    }
}

pub(in crate::supervisor) fn spawn(
    spec: ProgramSpec,
    limits: &[InstalledLimit],
    _fail_limit_install: bool,
) -> Result<(i32, Instant), NotStarted> {
    debug_assert!(limits.is_empty());
    let stdin = match spec.stdin {
        StdinSpec::Null => Stdio::null(),
        StdinSpec::Pipe(end) => end.into(),
        StdinSpec::Forward => unreachable!("stdin forwarding prepared before creation"),
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
    // Rust's Unix Command paths inherit the signal mask and ignored dispositions,
    // except SIGPIPE which std resets to SIG_DFL. init_process restores SIGCHLD.
    // This matches Linux: no blanket signal reset and no pre_exec hook. std sets
    // the process group before exec (spawn attributes or child-side setpgid),
    // duplicates CLOEXEC stdio, and owns its spawn-error channel. See the audit
    // in docs/design-macos-supervisor.md; re-audit when changing Rust toolchains.
    let _creation = creation_lock();
    let mut child = command.spawn().map_err(|error| NotStarted::CreateFailed {
        api: "spawn",
        raw_error: error.raw_os_error().unwrap_or(0),
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

/// One owned queue and a latched exit, both transferable to the cleanup service.
/// No kevent (which contains a raw udata pointer) is retained in this Send type.
#[derive(Debug)]
pub(in crate::supervisor) struct Observer {
    pub fd: AsyncFd<OwnedFd>,
    exited: AtomicBool,
    #[cfg(test)]
    pub registration_exited: bool,
}

fn queue() -> io::Result<OwnedFd> {
    let _creation = creation_lock();
    // SAFETY: kqueue takes no arguments and returns a new descriptor on success.
    let fd = unsafe { libc::kqueue() };
    if fd == -1 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: kqueue returned this uniquely owned descriptor.
    let fd = unsafe { OwnedFd::from_raw_fd(fd) };
    // SAFETY: fd remains owned; this changes only its descriptor flags.
    if unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_SETFD, libc::FD_CLOEXEC) } == -1 {
        return Err(io::Error::last_os_error());
    }
    Ok(fd)
}

pub(in crate::supervisor) fn probe_observer() -> io::Result<()> {
    queue().map(drop)
}

fn event(pid: i32) -> libc::kevent {
    libc::kevent {
        ident: pid as libc::uintptr_t,
        filter: libc::EVFILT_PROC,
        flags: libc::EV_ADD | libc::EV_RECEIPT,
        fflags: libc::NOTE_EXIT,
        data: 0,
        udata: std::ptr::null_mut(),
    }
}

fn receipt(pid: i32, count: i32, receipt: &libc::kevent) -> io::Result<bool> {
    if count != 1
        || receipt.ident != pid as libc::uintptr_t
        || receipt.filter != libc::EVFILT_PROC
        || receipt.flags & libc::EV_ERROR == 0
    {
        return Err(io::Error::from_raw_os_error(libc::EIO));
    }
    // `kevent` is packed on macOS: copy the field before matching on it.
    let data = receipt.data;
    match data {
        0 => Ok(false), // A registration acknowledgement, never an exit event.
        value if value == libc::ESRCH as libc::intptr_t => Ok(true),
        value => Err(io::Error::from_raw_os_error(
            i32::try_from(value)
                .ok()
                .filter(|value| *value > 0)
                .unwrap_or(libc::EIO),
        )),
    }
}

fn register(fd: &OwnedFd, pid: i32) -> io::Result<bool> {
    let change = event(pid);
    let timeout = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    loop {
        // SAFETY: zero is valid kevent storage; the returned count is validated.
        let mut output: libc::kevent = unsafe { std::mem::zeroed() };
        // SAFETY: fd is live, buffers each hold one event, timeout is initialized.
        // EV_RECEIPT applies the change without draining a pending NOTE_EXIT.
        let count = unsafe { libc::kevent(fd.as_raw_fd(), &change, 1, &mut output, 1, &timeout) };
        if count == -1 {
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            // Some kernels report ESRCH at the call rather than in a receipt.
            return if error.raw_os_error() == Some(libc::ESRCH) {
                Ok(true)
            } else {
                Err(error)
            };
        }
        match receipt(pid, count, &output) {
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            result => return result,
        }
    }
}

pub(in crate::supervisor) fn observer(pid: i32) -> io::Result<Observer> {
    let fd = queue()?;
    let exited = register(&fd, pid)?;
    #[cfg(test)]
    let registration_exited = exited;
    // Registration attaches to subsequent edges. Probe after attaching so an
    // immediate exit cannot be lost. WNOWAIT preserves root ownership.
    let exited = exited || observe(pid)?;
    // SAFETY: the descriptor stays valid and fixed inside AsyncFd for its entire
    // registration lifetime; no caller extracts or closes it behind the reactor.
    let fd = unsafe { AsyncFd::register_with_interest(fd, tokio::io::Interest::READABLE) }?;
    Ok(Observer {
        fd,
        exited: AtomicBool::new(exited),
        #[cfg(test)]
        registration_exited,
    })
}

fn exit_event(pid: i32, event: &libc::kevent) -> io::Result<bool> {
    if event.flags & libc::EV_ERROR != 0 {
        return Err(io::Error::from_raw_os_error(
            i32::try_from(event.data)
                .ok()
                .filter(|value| *value > 0)
                .unwrap_or(libc::EIO),
        ));
    }
    Ok(event.filter == libc::EVFILT_PROC
        && event.fflags & libc::NOTE_EXIT != 0
        && event.ident == pid as libc::uintptr_t)
}

/// Nonblocking check of the observer installed for this run. The queue only
/// accelerates observation: if it fails, one non-consuming polling step decides,
/// so a queue failure never abandons the still-owned root.
pub(in crate::supervisor) fn exit_check(observer: Option<&Observer>, pid: i32) -> io::Result<bool> {
    let Some(observer) = observer else {
        return observe(pid);
    };
    match queue_check(observer, pid) {
        Ok(exited) => Ok(exited),
        Err(_) => {
            let exited = observe(pid)?;
            if exited {
                observer.exited.store(true, Ordering::Relaxed);
            }
            Ok(exited)
        }
    }
}

/// Drain at most one queue event with a zero timeout; errors are reported so
/// the async observer can record them and switch to polling.
pub(in crate::supervisor) fn queue_check(observer: &Observer, pid: i32) -> io::Result<bool> {
    if observer.exited.load(Ordering::Relaxed) {
        return Ok(true);
    }
    let timeout = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    loop {
        // SAFETY: zero is valid kevent storage, only read after count == 1.
        let mut output: libc::kevent = unsafe { std::mem::zeroed() };
        // SAFETY: queue is live, no changes, one writable event, zero timeout.
        let count = unsafe {
            libc::kevent(
                observer.fd.get_ref().as_raw_fd(),
                std::ptr::null(),
                0,
                &mut output,
                1,
                &timeout,
            )
        };
        if count == -1 {
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return Err(error);
        }
        let exited = count == 1 && exit_event(pid, &output)?;
        if exited {
            observer.exited.store(true, Ordering::Relaxed);
        }
        return Ok(exited);
    }
}

pub(in crate::supervisor) fn observe(pid: i32) -> io::Result<bool> {
    observe_status(pid).map(|status| status.is_some())
}

pub(in crate::supervisor) fn observe_status(pid: i32) -> io::Result<Option<Termination>> {
    loop {
        // SAFETY: zero is a valid initial representation of siginfo_t; waitid
        // may leave it untouched on Darwin when WNOHANG finds no event.
        // Reinitialize on EVERY attempt, including retries after EINTR.
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
            if info.si_pid == 0 {
                return Ok(None);
            }
            let status = info.si_status;
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

pub(in crate::supervisor) fn kill_group(pgid: i32) -> io::Result<()> {
    // SAFETY: caller holds the live-run registry lock and checked capability;
    // the unreaped root pins this positive process-group id against reuse.
    if unsafe { libc::killpg(pgid, libc::SIGKILL) } == 0 {
        return Ok(());
    }
    let error = io::Error::last_os_error();
    // Darwin returns EPERM, not success, when the group's only remaining
    // member is the unreaped zombie leader (the normal state after the root
    // exits). The group consists of cpf's own children with cpf's
    // credentials, so EPERM here means no live member could be signalled;
    // the cleanup scan, not this call, decides whether members remain. A
    // setuid program inside the group would also yield EPERM and is likewise
    // left to the scan, never assumed dead.
    match error.raw_os_error() {
        Some(libc::ESRCH) | Some(libc::EPERM) => Ok(()),
        _ => Err(error),
    }
}

pub(in crate::supervisor) fn reap(pid: i32) -> io::Result<Option<(Termination, RawRusage)>> {
    #[cfg(test)]
    if REAP_ZERO.with(|injection| {
        let (injected_pid, remaining) = injection.get();
        if injected_pid != pid || remaining == 0 {
            return false;
        }
        injection.set((pid, remaining - 1));
        true
    }) {
        return Ok(None);
    }
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
                maxrss_bytes: usage.ru_maxrss.max(0) as u64,
                user: duration(usage.ru_utime),
                system: duration(usage.ru_stime),
            },
        )));
    }
}

/// Stop a session root while preserving descendants for the final transport drain.
#[allow(dead_code)] // Native interface; connected sessions remain Linux-only.
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

#[path = "macos_proc.rs"]
mod cleanup;
#[cfg(test)]
pub(in crate::supervisor) use cleanup::test_members;
pub(in crate::supervisor) use cleanup::{CleanupScan, cleanup_scan, probe_cleanup};

/// Run before worker startup, like Linux's procfs fallback. Enumeration is never
/// performed in pre_exec and never closes std's private spawn-error descriptor.
pub(in crate::supervisor) fn init_process() -> io::Result<()> {
    // Own the directory stream explicitly so its descriptor can be excluded.
    // SAFETY: the constant is a valid NUL-terminated path.
    let directory = unsafe { libc::opendir(c"/dev/fd".as_ptr()) };
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
            *libc::__error() = 0;
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

#[cfg(test)]
thread_local! {
    // Per-thread and per-pid: the cleanup service and concurrent tests are never
    // affected. Current-thread runtime tests inject only their owned root.
    static REAP_ZERO: std::cell::Cell<(i32, u32)> = const { std::cell::Cell::new((0, 0)) };
}

#[cfg(test)]
pub(in crate::supervisor) fn inject_reap_zero(pid: i32, count: u32) {
    REAP_ZERO.with(|injection| injection.set((pid, count)));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn receipts_are_acknowledgements_and_exit_events_are_strictly_validated() {
        let pid = 123;
        let mut ack = event(pid);
        ack.flags = libc::EV_ERROR;
        assert!(!receipt(pid, 1, &ack).unwrap());
        ack.data = libc::ESRCH as libc::intptr_t;
        assert!(receipt(pid, 1, &ack).unwrap());
        for errno in [libc::EACCES, libc::EINTR, libc::ENOMEM] {
            ack.data = errno as libc::intptr_t;
            assert_eq!(
                receipt(pid, 1, &ack).unwrap_err().raw_os_error(),
                Some(errno)
            );
        }
        assert!(receipt(pid, 0, &ack).is_err());
        ack.ident += 1;
        assert!(receipt(pid, 1, &ack).is_err());
        ack = event(pid);
        assert!(receipt(pid, 1, &ack).is_err()); // missing EV_ERROR
        ack.flags = libc::EV_ERROR;
        ack.filter = libc::EVFILT_READ;
        assert!(receipt(pid, 1, &ack).is_err());

        let mut exit = event(pid);
        exit.flags = 0;
        assert!(exit_event(pid, &exit).unwrap());
        exit.ident += 1;
        assert!(!exit_event(pid, &exit).unwrap());
        exit.ident = pid as libc::uintptr_t;
        exit.filter = libc::EVFILT_READ;
        assert!(!exit_event(pid, &exit).unwrap());
        exit.filter = libc::EVFILT_PROC;
        exit.fflags = 0;
        assert!(!exit_event(pid, &exit).unwrap());
        exit.flags = libc::EV_ERROR;
        exit.data = libc::EIO as libc::intptr_t;
        assert_eq!(
            exit_event(pid, &exit).unwrap_err().raw_os_error(),
            Some(libc::EIO)
        );
    }

    #[test]
    fn queue_is_cloexec_and_transfer_types_are_send() {
        fn is_send<T: Send>() {}
        is_send::<Observer>();
        is_send::<CleanupScan>();
        let queue = queue().unwrap();
        // SAFETY: the owned queue remains live; F_GETFD only reads its flags.
        assert_ne!(
            unsafe { libc::fcntl(queue.as_raw_fd(), libc::F_GETFD) } & libc::FD_CLOEXEC,
            0
        );
    }

    #[test]
    fn startup_sanitizes_inherited_descriptors() {
        let _lock = crate::supervisor::root::unix::tests::TEST_LOCK
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let file = File::open("/dev/null").unwrap();
        // SAFETY: the file is live; model an inherited descriptor without CLOEXEC.
        assert_eq!(
            unsafe { libc::fcntl(file.as_raw_fd(), libc::F_SETFD, 0) },
            0
        );
        crate::supervisor::init_process().unwrap();
        // SAFETY: file remains live; this reads only descriptor flags.
        assert_ne!(
            unsafe { libc::fcntl(file.as_raw_fd(), libc::F_GETFD) } & libc::FD_CLOEXEC,
            0
        );
    }
}
