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
    #[cfg(all(
        feature = "storage-test-hooks",
        any(target_os = "linux", target_os = "macos")
    ))]
    crate::supervisor::test_hooks::before_pipe()?;
    #[cfg(target_os = "macos")]
    let _creation = super::macos::creation_lock();
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

/// Piped stdin: the child end stays blocking, only the parent writer is async.
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(in crate::supervisor) type InputEnd = OwnedFd;

#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(in crate::supervisor) fn input_pipe() -> io::Result<(InputEnd, tokio::net::unix::pipe::Sender)>
{
    #[cfg(feature = "storage-test-hooks")]
    crate::supervisor::test_hooks::before_pipe()?;
    let mut fds = [-1; 2];
    #[cfg(target_os = "macos")]
    let _creation = super::macos::creation_lock();
    #[cfg(target_os = "linux")]
    // SAFETY: storage holds two descriptors; successful outputs are uniquely owned.
    let result = unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC) };
    #[cfg(target_os = "macos")]
    // SAFETY: successful outputs are new descriptors, protected by creation_lock.
    let result = unsafe { libc::pipe(fds.as_mut_ptr()) };
    if result == -1 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: pipe creation just returned these distinct owned descriptors.
    let (input, output) = unsafe { (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) };
    #[cfg(target_os = "macos")]
    for fd in [&input, &output] {
        // SAFETY: live descriptors owned here; no child can be created concurrently.
        if unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_SETFD, libc::FD_CLOEXEC) } == -1 {
            return Err(io::Error::last_os_error());
        }
    }
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
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(in crate::supervisor) fn direct_pipe() -> io::Result<(InputEnd, Writer)> {
    #[cfg(feature = "storage-test-hooks")]
    crate::supervisor::test_hooks::before_pipe()?;
    #[cfg(target_os = "macos")]
    let _creation = super::macos::creation_lock();
    let mut fds = [-1; 2];
    #[cfg(target_os = "linux")]
    // SAFETY: writable storage for two new, uniquely owned CLOEXEC descriptors.
    let result = unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC) };
    #[cfg(target_os = "macos")]
    // SAFETY: writable storage; creation_lock prevents inheritance before CLOEXEC.
    let result = unsafe { libc::pipe(fds.as_mut_ptr()) };
    if result == -1 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: successful pipe creation returned two distinct owned descriptors.
    let (input, output) = unsafe { (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) };
    #[cfg(target_os = "macos")]
    for fd in [&input, &output] {
        // SAFETY: each descriptor is live and spawn is serialized until marked.
        if unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_SETFD, libc::FD_CLOEXEC) } == -1 {
            return Err(io::Error::last_os_error());
        }
    }
    Ok((input, output))
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

/// Raw console output bypasses Rust's stream locks/buffering: a blocked console
/// forwarder must not hold the lock needed by the final diagnostic writer.
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(in crate::supervisor) fn console_write(
    stream: crate::supervisor::Stream,
    bytes: &[u8],
) -> io::Result<usize> {
    let fd = match stream {
        crate::supervisor::Stream::Stdout => 1,
        crate::supervisor::Stream::Stderr => 2,
    };
    if fd == 1 && crate::process::stdout_closed() {
        return Err(io::Error::from_raw_os_error(libc::EBADF));
    }
    // SAFETY: borrows an initialized standard descriptor and a valid byte slice.
    // SIGPIPE is ignored at startup; EPIPE is returned to the forwarding worker.
    let result = unsafe { libc::write(fd, bytes.as_ptr().cast(), bytes.len()) };
    if result < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(result as usize)
    }
}

#[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
mod tests {
    use super::*;

    #[test]
    fn direct_pipe_keeps_both_child_ends_blocking_and_cloexec() {
        let (input, output) = direct_pipe().unwrap();
        for fd in [&input, &output] {
            // SAFETY: the descriptors are live and owned throughout the checks.
            let descriptor_flags = unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_GETFD) };
            // SAFETY: F_GETFL only reads the live descriptor's file status flags.
            let status_flags = unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_GETFL) };
            assert_ne!(descriptor_flags, -1);
            assert_ne!(status_flags, -1);
            assert_ne!(descriptor_flags & libc::FD_CLOEXEC, 0);
            assert_eq!(status_flags & libc::O_NONBLOCK, 0);
        }
    }
}
