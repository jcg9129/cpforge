use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::process::Stdio;
use tokio::io::AsyncReadExt;
use tokio::net::unix::pipe::Receiver;

pub(in crate::supervisor) type Reader = Receiver;
pub(in crate::supervisor) type Writer = OwnedFd;

pub(in crate::supervisor) fn reset_sigchld() -> io::Result<()> {
    // SAFETY: zero is valid initial storage for sigaction; sigemptyset
    // initializes the signal mask before the action is installed.
    let mut action: libc::sigaction = unsafe { std::mem::zeroed() };
    action.sa_sigaction = libc::SIG_DFL;
    action.sa_flags = 0;
    // SAFETY: action owns writable mask storage.
    if unsafe { libc::sigemptyset(&mut action.sa_mask) } == -1 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: action is fully initialized, with SIG_DFL and no SA_NOCLDWAIT.
    if unsafe { libc::sigaction(libc::SIGCHLD, &action, std::ptr::null_mut()) } == -1 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

pub(in crate::supervisor) fn pipe() -> io::Result<(Reader, Writer)> {
    let mut fds = [-1; 2];
    #[cfg(not(target_os = "macos"))]
    // SAFETY: fds points to two writable integers. On success they are new,
    // uniquely owned descriptors, both atomically marked close-on-exec.
    let result = unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC) };
    #[cfg(target_os = "macos")]
    // macOS has no pipe2. The spawn backend must serialize pipe+fcntl with process
    // creation to prevent inheritance during the brief pre-CLOEXEC interval.
    // SAFETY: fds points to two writable integers; successful outputs are new.
    let result = unsafe { libc::pipe(fds.as_mut_ptr()) };
    if result == -1 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: successful pipe creation returned two distinct, owned descriptors.
    // Wrapping both before any fallible step guarantees cleanup on every error.
    let (parent, child) = unsafe { (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) };
    #[cfg(target_os = "macos")]
    for fd in [&parent, &child] {
        // SAFETY: fd is live and borrowed; F_SETFD only changes descriptor flags.
        if unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_SETFD, libc::FD_CLOEXEC) } == -1 {
            return Err(io::Error::last_os_error());
        }
    }
    // SAFETY: parent is live; F_GETFL reads its file status flags.
    let flags = unsafe { libc::fcntl(parent.as_raw_fd(), libc::F_GETFL) };
    if flags == -1 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: parent is live; this preserves its flags and enables non-blocking
    // reads. The distinct child write end remains blocking and close-on-exec.
    if unsafe { libc::fcntl(parent.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK) } == -1 {
        return Err(io::Error::last_os_error());
    }
    Ok((Receiver::from_owned_fd(parent)?, child))
}

pub(in crate::supervisor) async fn read(reader: &mut Reader, buf: &mut [u8]) -> io::Result<usize> {
    // Tokio handles EAGAIN and spurious readiness; the shared layer retries EINTR.
    reader.read(buf).await
}

pub(in crate::supervisor) fn into_stdio(writer: Writer) -> Stdio {
    writer.into()
}

/// Session stdin: the child end stays blocking, only the parent writer is async.
#[cfg(target_os = "linux")]
pub(in crate::supervisor) type InputEnd = OwnedFd;

#[cfg(target_os = "linux")]
pub(in crate::supervisor) fn input_pipe() -> io::Result<(InputEnd, tokio::net::unix::pipe::Sender)>
{
    let mut fds = [-1; 2];
    // SAFETY: storage holds two descriptors; successful outputs are uniquely owned.
    if unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC) } == -1 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: pipe2 just returned these distinct owned descriptors.
    let (input, output) = unsafe { (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) };
    // SAFETY: output is live; only the parent end becomes nonblocking.
    if unsafe { libc::fcntl(output.as_raw_fd(), libc::F_SETFL, libc::O_NONBLOCK) } == -1 {
        return Err(io::Error::last_os_error());
    }
    Ok((
        input,
        tokio::net::unix::pipe::Sender::from_owned_fd(output)?,
    ))
}

/// Both dialogue ends belong to children; neither end is nonblocking.
#[cfg(target_os = "linux")]
pub(in crate::supervisor) fn direct_pipe() -> io::Result<(InputEnd, Writer)> {
    let mut fds = [-1; 2];
    // SAFETY: writable storage for two new, uniquely owned CLOEXEC descriptors.
    if unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC) } == -1 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: successful pipe2 returned two distinct owned descriptors.
    Ok(unsafe { (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) })
}

#[cfg(all(target_os = "linux", debug_assertions))]
pub(in crate::supervisor) fn resize_pipe(
    pipe: &tokio::net::unix::pipe::Sender,
    bytes: usize,
) -> io::Result<()> {
    // SAFETY: borrowed live pipe; F_SETPIPE_SZ only adjusts its buffer capacity.
    if unsafe { libc::fcntl(pipe.as_raw_fd(), libc::F_SETPIPE_SZ, bytes as libc::c_int) } == -1 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}
