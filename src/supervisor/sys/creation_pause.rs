//! Test-only fork/exec handshake. The child executes only async-signal-safe I/O.
use std::io::{self, Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt;
use std::process::Command;
use tokio::io::unix::AsyncFd;

#[derive(Debug)]
pub(in crate::supervisor) struct CreationPause {
    parent: UnixStream,
    child: UnixStream,
}

impl CreationPause {
    pub(in crate::supervisor) fn new() -> io::Result<Self> {
        let (parent, child) = UnixStream::pair()?;
        parent.set_nonblocking(true)?;
        Ok(Self { parent, child })
    }

    pub(in crate::supervisor) async fn entered(&self) {
        let socket = self.parent.try_clone().unwrap();
        // SAFETY: the cloned stream owns its descriptor and keeps it valid and
        // unchanged until the AsyncFd drops; it is never closed behind its back.
        let fd = unsafe { AsyncFd::register_with_interest(socket, tokio::io::Interest::READABLE) }
            .unwrap();
        loop {
            let mut ready = fd.readable().await.unwrap();
            match ready.try_io(|fd| {
                let mut socket = fd.get_ref();
                socket.read(&mut [0])
            }) {
                Ok(Ok(1)) => return,
                Ok(result) => panic!("creation handshake failed: {result:?}"),
                Err(_) => continue,
            }
        }
    }

    pub(in crate::supervisor) fn release(&self) {
        (&self.parent).write_all(&[1]).unwrap();
    }

    pub(in crate::supervisor) fn install(&self, command: &mut Command) {
        let fd = self.child.as_raw_fd();
        // SAFETY: the gate owns fd until spawn returns. After fork this closure
        // uses only read/write and errno, without locks, allocation, or unwinding.
        unsafe {
            command.pre_exec(move || {
                let mut byte = 1_u8;
                loop {
                    let n = libc::write(fd, (&byte as *const u8).cast(), 1);
                    if n == 1 {
                        break;
                    }
                    let error = io::Error::last_os_error();
                    if error.raw_os_error() != Some(libc::EINTR) {
                        return Err(error);
                    }
                }
                loop {
                    let n = libc::read(fd, (&mut byte as *mut u8).cast(), 1);
                    if n == 1 {
                        return Ok(());
                    }
                    if n == 0 {
                        return Err(io::Error::from_raw_os_error(libc::EPIPE));
                    }
                    let error = io::Error::last_os_error();
                    if error.raw_os_error() != Some(libc::EINTR) {
                        return Err(error);
                    }
                }
            });
        }
    }
}

/// CLI seam: the harness owns the peer, so the child stays inside pre_exec even
/// after force exit closes every descriptor in cpf. No environment lookup,
/// allocation or Rust stream lock runs in the forked child.
pub(in crate::supervisor) fn install_cli(command: &mut Command) -> io::Result<Option<UnixStream>> {
    let Some(path) = std::env::var_os("CPF_STORAGE_CREATION_SOCKET") else {
        return Ok(None);
    };
    let socket = UnixStream::connect(path)?;
    let fd = socket.as_raw_fd();
    // SAFETY: socket is kept alive until spawn returns. The child uses only
    // getpid, read, write and errno; the test harness pins it until released.
    unsafe {
        command.pre_exec(move || {
            let pid = libc::getpid().to_ne_bytes();
            let mut sent = 0;
            while sent < pid.len() {
                let n = libc::write(fd, pid[sent..].as_ptr().cast(), pid.len() - sent);
                if n > 0 {
                    sent += n as usize;
                } else {
                    let error = io::Error::last_os_error();
                    if error.raw_os_error() != Some(libc::EINTR) {
                        return Err(error);
                    }
                }
            }
            loop {
                let mut byte = 0_u8;
                match libc::read(fd, (&mut byte as *mut u8).cast(), 1) {
                    1 => return Ok(()),
                    0 => return Err(io::Error::from_raw_os_error(libc::EPIPE)),
                    _ => {
                        let error = io::Error::last_os_error();
                        if error.raw_os_error() != Some(libc::EINTR) {
                            return Err(error);
                        }
                    }
                }
            }
        });
    }
    Ok(Some(socket))
}
