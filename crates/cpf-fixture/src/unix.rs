use super::{HANDSHAKE_TIMEOUT, MAX_MESSAGE, channel_value, retry_pause};
use crate::script::Stream;
use std::io;
use std::os::fd::{AsRawFd, RawFd};
use std::os::unix::net::UnixDatagram;
use std::os::unix::process::CommandExt;
use std::process::{Child, Command};
use std::time::{Duration, Instant};

#[derive(Debug)]
pub(crate) struct Server(UnixDatagram);

#[derive(Debug)]
pub(crate) struct ChildChannel(UnixDatagram);

pub(crate) fn channel() -> io::Result<(Server, ChildChannel)> {
    let (server, child) = UnixDatagram::pair()?;
    Ok((Server(server), ChildChannel(child)))
}

impl ChildChannel {
    pub(crate) fn configure(&self, command: &mut Command) {
        let fd = self.0.as_raw_fd();
        command.env("CPF_FIXTURE_CTRL", fd.to_string());
        command.env("CPF_FIXTURE_ACK", fd.to_string());
        // SAFETY: pre_exec uses only async-signal-safe fcntl on a live descriptor;
        // no allocation, locks, or access to the parent's Rust state occurs.
        unsafe {
            command.pre_exec(move || {
                if libc::fcntl(fd, libc::F_SETFD, 0) == -1 {
                    return Err(io::Error::last_os_error());
                }
                Ok(())
            });
        }
    }
}

impl Server {
    pub(crate) fn receive(&self, within: Duration) -> io::Result<String> {
        self.0.set_read_timeout(Some(within))?;
        let mut buffer = [0; MAX_MESSAGE + 1];
        let count = loop {
            match self.0.recv(&mut buffer) {
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                result => break result?,
            }
        };
        if count > MAX_MESSAGE {
            return Err(io::Error::other("oversized READY message"));
        }
        String::from_utf8(buffer[..count].to_vec()).map_err(io::Error::other)
    }

    pub(crate) fn acknowledge(&self, pid: u32) -> io::Result<()> {
        self.0.set_write_timeout(Some(HANDSHAKE_TIMEOUT))?;
        self.0.send(format!("ACK {pid}\n").as_bytes())?;
        Ok(())
    }
}

fn inherited_fd(name: &str) -> io::Result<RawFd> {
    i32::try_from(channel_value(name)?)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "control fd out of range"))
}

fn send(fd: RawFd, bytes: &[u8]) -> io::Result<()> {
    let until = Instant::now() + HANDSHAKE_TIMEOUT;
    loop {
        // SAFETY: the slice is valid for its length; send borrows the inherited fd.
        let result =
            unsafe { libc::send(fd, bytes.as_ptr().cast(), bytes.len(), libc::MSG_DONTWAIT) };
        if result == bytes.len() as isize {
            return Ok(());
        }
        let error = io::Error::last_os_error();
        if !matches!(
            error.kind(),
            io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
        ) {
            return Err(error);
        }
        retry_pause(until)?;
    }
}

fn receive(fd: RawFd, peek: bool, buffer: &mut [u8]) -> io::Result<usize> {
    let flags = libc::MSG_DONTWAIT | if peek { libc::MSG_PEEK } else { 0 };
    // SAFETY: the mutable slice is valid for its length; recv borrows the fd.
    let result = unsafe { libc::recv(fd, buffer.as_mut_ptr().cast(), buffer.len(), flags) };
    if result < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(result as usize)
    }
}

pub(crate) fn ready() -> io::Result<()> {
    let pid = std::process::id();
    let (start_id, _) = process_identity(pid)?;
    send(
        inherited_fd("CPF_FIXTURE_CTRL")?,
        format!("READY {pid} {start_id}\n").as_bytes(),
    )?;
    let fd = inherited_fd("CPF_FIXTURE_ACK")?;
    let expected = format!("ACK {pid}\n");
    let until = Instant::now() + HANDSHAKE_TIMEOUT;
    let mut buffer = [0; MAX_MESSAGE];
    loop {
        match receive(fd, true, &mut buffer) {
            Ok(n) if buffer[..n] == *expected.as_bytes() => {
                // Only the process named in this datagram consumes it. Other
                // processes merely peek, so this cannot consume another's ACK.
                receive(fd, false, &mut buffer)?;
                return Ok(());
            }
            Ok(_) => {}
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                ) => {}
            Err(error) => return Err(error),
        }
        retry_pause(until)?;
    }
}

#[derive(Debug)]
pub(crate) struct Tree {
    pgid: i32,
}

impl Tree {
    pub(crate) fn new() -> io::Result<Self> {
        Ok(Self { pgid: 0 })
    }

    pub(crate) fn configure(&self, command: &mut Command) {
        command.process_group(0);
    }

    pub(crate) fn attach(&mut self, child: &Child) -> io::Result<()> {
        self.pgid = child.id() as i32;
        Ok(())
    }

    pub(crate) fn kill(&self) -> io::Result<()> {
        if self.pgid == 0 {
            return Ok(()); // setup failed before a child group existed
        }
        // SAFETY: pgid is a child-created, positive process group ID. A negative
        // kill target addresses the group, never the harness's process group.
        let result = unsafe { libc::kill(-self.pgid, libc::SIGKILL) };
        if result == 0 || io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH) {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) struct Process;

impl Process {
    pub(crate) fn open(pid: u32, expected: u64) -> io::Result<Self> {
        if process_identity(pid)?.0 != expected {
            return Err(io::Error::other("READY identity changed before ACK"));
        }
        Ok(Self)
    }

    pub(crate) fn is_live(&self, pid: u32, start_id: u64) -> bool {
        // SAFETY: signal zero only probes the process; no pointers are involved.
        let result = unsafe { libc::kill(pid as i32, 0) };
        if result == -1 && io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH) {
            return false;
        }
        match process_identity(pid) {
            Ok((current, zombie)) => current == start_id && !zombie,
            // macOS proc_pidinfo with arg=0 excludes zombies and returns ESRCH,
            // even though kill(pid, 0) succeeds. ESRCH is not ErrorKind::NotFound.
            Err(error) if error.raw_os_error() == Some(libc::ESRCH) => false,
            // An inaccessible identity is conservatively still live.
            Err(error) => error.kind() != io::ErrorKind::NotFound,
        }
    }
}

#[cfg(target_os = "linux")]
pub(crate) fn process_identity(pid: u32) -> io::Result<(u64, bool)> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat"))?;
    let fields: Vec<_> = stat
        .rsplit_once(')')
        .ok_or_else(|| io::Error::other("invalid /proc stat comm"))?
        .1
        .split_whitespace()
        .collect();
    let start_id = fields
        .get(19) // suffix starts at field 3; starttime is field 22.
        .and_then(|text| text.parse().ok())
        .ok_or_else(|| io::Error::other("invalid /proc stat starttime"))?;
    Ok((start_id, fields.first() == Some(&"Z")))
}

#[cfg(target_os = "macos")]
pub(crate) fn process_identity(pid: u32) -> io::Result<(u64, bool)> {
    let mut info = std::mem::MaybeUninit::<libc::proc_bsdinfo>::uninit();
    let size = std::mem::size_of::<libc::proc_bsdinfo>() as i32;
    // SAFETY: proc_pidinfo writes at most size bytes into the correctly sized
    // output; initialization is checked by requiring the exact return size.
    let result = unsafe {
        libc::proc_pidinfo(
            pid as i32,
            libc::PROC_PIDTBSDINFO,
            0,
            info.as_mut_ptr().cast(),
            size,
        )
    };
    if result != size {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: proc_pidinfo returned the full struct above.
    let info = unsafe { info.assume_init() };
    Ok((
        info.pbi_start_tvsec * 1_000_000 + info.pbi_start_tvusec,
        info.pbi_status == libc::SZOMB,
    ))
}

pub(crate) fn validate_signal(name: &str) -> Result<(), String> {
    match name {
        "KILL" | "TERM" | "SEGV" | "ABRT" => Ok(()),
        _ => Err(format!("unsupported signal: {name}")),
    }
}

pub(crate) fn signal(name: &str) -> io::Result<()> {
    let signal = match name {
        "KILL" => libc::SIGKILL,
        "TERM" => libc::SIGTERM,
        "SEGV" => libc::SIGSEGV,
        "ABRT" => libc::SIGABRT,
        _ => unreachable!("validated signal"),
    };
    // Never leave core dumps behind in the test's working directory.
    let no_core = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    // SAFETY: the rlimit value is fully initialised; lowering RLIMIT_CORE only
    // affects this fixture process.
    unsafe {
        libc::setrlimit(libc::RLIMIT_CORE, &no_core);
    }
    // SAFETY: raise sends a validated signal to this process, without pointers.
    if unsafe { libc::raise(signal) } == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

pub(crate) fn exit(code: u32) -> ! {
    std::process::exit(code as i32)
}

pub(crate) fn close(stream: Stream) -> io::Result<()> {
    let fd = match stream {
        Stream::Stdin => 0,
        Stream::Stdout => 1,
        Stream::Stderr => 2,
    };
    // SAFETY: this script explicitly gives up this standard descriptor. No Rust
    // owned File is constructed from it, so no second owning close is possible.
    if unsafe { libc::close(fd) } == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

pub(crate) fn escape() -> io::Result<()> {
    // SAFETY: setsid has no pointer arguments and changes only this process.
    if unsafe { libc::setsid() } == -1 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

pub(crate) fn ignore_term() -> io::Result<()> {
    // SAFETY: SIG_IGN is a valid signal disposition, requiring no callback.
    if unsafe { libc::signal(libc::SIGTERM, libc::SIG_IGN) } == libc::SIG_ERR {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

pub(crate) fn startup() -> io::Result<()> {
    Ok(())
}

/// Explicit script opt-in: Rust's runtime otherwise ignores SIGPIPE.
pub(crate) fn default_sigpipe() -> io::Result<()> {
    // SAFETY: SIG_DFL is a valid disposition; this changes only this fixture.
    if unsafe { libc::signal(libc::SIGPIPE, libc::SIG_DFL) } == libc::SIG_ERR {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}
