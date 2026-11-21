//! Process-wide signal listener installed only by job commands. Handlers publish
//! notifications; the control thread performs cancellation and forced cleanup.
use std::io;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU64, Ordering};
use std::thread::JoinHandle;

use crate::exit::ExitCode;
use crate::jobs::engine::{CancelSource, CancelState};

static ACTIVE: AtomicBool = AtomicBool::new(false);
static FIRST: AtomicI32 = AtomicI32::new(0);
static FIRST_AT: AtomicU64 = AtomicU64::new(0);
static FORCE: AtomicI32 = AtomicI32::new(0);
static EXITING: AtomicBool = AtomicBool::new(false);

pub fn force_exit_started() -> bool {
    EXITING.load(Ordering::Acquire)
}

/// Read by the verdict lock as well as the listener, closing the race between
/// an OS notification and F. No user cancellation logic runs in the handler.
pub(crate) fn pending() -> Option<CancelSource> {
    match FIRST.load(Ordering::Acquire) {
        1 => Some(CancelSource::Console),
        2 => Some(CancelSource::Terminate),
        _ => None,
    }
}
fn notification(source: i32) {
    let now = platform::millis();
    if FIRST
        .compare_exchange(0, source, Ordering::AcqRel, Ordering::Acquire)
        .is_ok()
    {
        FIRST_AT.store(now, Ordering::Release);
    } else {
        let first = FIRST_AT.load(Ordering::Acquire);
        if first != 0 && now.saturating_sub(first) >= 100 {
            FORCE.store(source, Ordering::Release);
        }
    }
    platform::wake();
}

#[derive(Debug)]
pub struct CancelSignals {
    listener: platform::Listener,
    stop: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
    cancel: CancelState,
}
impl CancelSignals {
    /// Keep this guard alive through phase D and the final stderr summary.
    /// A second installation fails rather than replacing another job's handler.
    pub fn install(cancel: CancelState) -> io::Result<Self> {
        if ACTIVE
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "a job signal listener is already installed",
            ));
        }
        FIRST.store(0, Ordering::Release);
        FIRST_AT.store(0, Ordering::Release);
        FORCE.store(0, Ordering::Release);
        let listener = match platform::Listener::install() {
            Ok(listener) => listener,
            Err(error) => {
                ACTIVE.store(false, Ordering::Release);
                return Err(error);
            }
        };
        cancel.bind_signals(true);
        let stop = Arc::new(AtomicBool::new(false));
        let stopping = stop.clone();
        let state = cancel.clone();
        let mut reader = match listener.reader() {
            Ok(reader) => reader,
            Err(error) => {
                cancel.bind_signals(false);
                drop(listener);
                ACTIVE.store(false, Ordering::Release);
                return Err(error);
            }
        };
        let worker = match std::thread::Builder::new()
            .name("cpf-control".into())
            .spawn(move || {
                let mut observed = false;
                while !stopping.load(Ordering::Acquire) {
                    platform::wait(&mut reader);
                    let forcing = FORCE.load(Ordering::Acquire);
                    if forcing != 0 {
                        EXITING.store(true, Ordering::Release);
                        crate::supervisor::force_kill_active();
                        terminate_source(if forcing == 2 {
                            CancelSource::Terminate
                        } else {
                            CancelSource::Console
                        });
                    }
                    // Never wait for the verdict lock on the force-exit thread.
                    // The owner also folds pending notifications into every
                    // verdict-lock acquisition, including the F snapshot.
                    state.poll_signals();
                    if !observed && FIRST_AT.load(Ordering::Acquire) != 0 {
                        observed = true;
                        crate::storage::crash::signal_observed();
                    }
                }
            }) {
            Ok(worker) => worker,
            Err(error) => {
                cancel.bind_signals(false);
                drop(listener);
                ACTIVE.store(false, Ordering::Release);
                return Err(error);
            }
        };
        Ok(Self {
            listener,
            stop,
            worker: Some(worker),
            cancel,
        })
    }
    /// Apply §5.3 only when the precedence selector chose cancellation. Delivery
    /// and internal failures therefore suppress orderly signal re-raising.
    pub fn finish(self, status: ExitCode) -> ExitCode {
        let source = self.cancel.cancellation().map(|intent| intent.source);
        if status == ExitCode::Cancelled {
            if let Some(source) = source {
                if cfg!(unix) && source != CancelSource::Api {
                    terminate_source(source);
                }
            }
        }
        status
    }
}
impl Drop for CancelSignals {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        platform::wake();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
        self.listener.restore();
        self.cancel.bind_signals(false);
        ACTIVE.store(false, Ordering::Release);
    }
}
fn terminate_source(source: CancelSource) -> ! {
    #[cfg(unix)]
    if source != CancelSource::Api {
        let signal = if source == CancelSource::Terminate {
            libc::SIGTERM
        } else {
            libc::SIGINT
        };
        // SAFETY: restore and unblock only the signal that will terminate this
        // process. These operations and raise are async-signal-safe as well.
        // `raise` targets this thread, where the signal is now unblocked with
        // its default action, so it terminates the process before returning;
        // `kill(getpid())` may be delivered asynchronously to another thread
        // (macOS), letting the `_exit` below win.
        unsafe {
            libc::signal(signal, libc::SIG_DFL);
            let mut mask: libc::sigset_t = std::mem::zeroed();
            libc::sigemptyset(&mut mask);
            libc::sigaddset(&mut mask, signal);
            libc::pthread_sigmask(libc::SIG_UNBLOCK, &mask, std::ptr::null_mut());
            libc::raise(signal);
        }
    }
    #[cfg(not(unix))]
    let _ = source;
    // No destructors or buffered writes on the force-exit path.
    #[cfg(unix)]
    unsafe {
        libc::_exit(ExitCode::Cancelled as i32)
    }
    #[cfg(windows)]
    unsafe {
        windows_sys::Win32::System::Threading::ExitProcess(ExitCode::Cancelled as u32)
    }
}

#[cfg(unix)]
mod platform {
    use super::*;
    use std::fs::File;
    use std::io::Read;
    use std::os::fd::{AsRawFd, FromRawFd};
    static WRITE_FD: AtomicI32 = AtomicI32::new(-1);
    extern "C" fn handler(signal: libc::c_int) {
        // Preserve interrupted code's errno across the nonblocking pipe write.
        #[cfg(target_os = "linux")]
        let errno = unsafe { libc::__errno_location() };
        #[cfg(not(target_os = "linux"))]
        let errno = unsafe { libc::__error() };
        let saved = unsafe { *errno };
        notification(if signal == libc::SIGTERM { 2 } else { 1 });
        unsafe {
            *errno = saved;
        }
    }
    pub(super) fn millis() -> u64 {
        let mut time: libc::timespec = unsafe { std::mem::zeroed() };
        unsafe {
            libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut time);
        }
        (time.tv_sec as u64)
            .saturating_mul(1000)
            .saturating_add(time.tv_nsec as u64 / 1_000_000)
            .max(1)
    }
    pub(super) fn wake() {
        let fd = WRITE_FD.load(Ordering::Acquire);
        if fd >= 0 {
            // SAFETY: the listener owns this nonblocking pipe descriptor. A
            // full pipe is already a pending wakeup; static atomics retain it.
            unsafe {
                libc::write(fd, [1_u8].as_ptr().cast(), 1);
            }
        }
    }
    pub(super) struct Listener {
        read: File,
        _write: File,
        old: [libc::sigaction; 2],
        restored: bool,
    }
    impl std::fmt::Debug for Listener {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.debug_struct("Listener")
                .field("read", &self.read)
                .field("restored", &self.restored)
                .finish_non_exhaustive()
        }
    }
    impl Listener {
        pub(super) fn install() -> io::Result<Self> {
            let mut fds = [0; 2];
            // Linux creates both ends close-on-exec atomically. macOS has no
            // pipe2, so the pipe and its FD_CLOEXEC are made under the
            // supervisor's creation lock, never between a child's fork and exec.
            #[cfg(target_os = "linux")]
            // SAFETY: valid two-element output array; ownership transferred to File.
            if unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC) } < 0 {
                return Err(io::Error::last_os_error());
            }
            #[cfg(not(target_os = "linux"))]
            let _creation = {
                #[cfg(target_os = "macos")]
                let guard = crate::supervisor::creation_lock();
                // SAFETY: valid two-element output array; ownership transferred to File.
                if unsafe { libc::pipe(fds.as_mut_ptr()) } < 0 {
                    return Err(io::Error::last_os_error());
                }
                for fd in fds {
                    // SAFETY: both descriptors were just created and are owned here.
                    if unsafe { libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) } < 0 {
                        let error = io::Error::last_os_error();
                        // SAFETY: closing the descriptors this call created.
                        unsafe {
                            libc::close(fds[0]);
                            libc::close(fds[1]);
                        }
                        return Err(error);
                    }
                }
                #[cfg(target_os = "macos")]
                guard
            };
            // SAFETY: the descriptors were created above and are owned here.
            let read = unsafe { File::from_raw_fd(fds[0]) };
            let write = unsafe { File::from_raw_fd(fds[1]) };
            if unsafe { libc::fcntl(fds[1], libc::F_SETFL, libc::O_NONBLOCK) } < 0 {
                return Err(io::Error::last_os_error());
            }
            WRITE_FD.store(write.as_raw_fd(), Ordering::Release);
            let mut old: [libc::sigaction; 2] = unsafe { std::mem::zeroed() };
            let mut action: libc::sigaction = unsafe { std::mem::zeroed() };
            action.sa_sigaction = handler as *const () as usize;
            action.sa_flags = libc::SA_RESTART;
            unsafe {
                libc::sigemptyset(&mut action.sa_mask);
                libc::sigaddset(&mut action.sa_mask, libc::SIGINT);
                libc::sigaddset(&mut action.sa_mask, libc::SIGTERM);
            }
            for (index, signal) in [libc::SIGINT, libc::SIGTERM].into_iter().enumerate() {
                if unsafe { libc::sigaction(signal, &action, &mut old[index]) } < 0 {
                    let error = io::Error::last_os_error();
                    if index != 0 {
                        unsafe {
                            libc::sigaction(libc::SIGINT, &old[0], std::ptr::null_mut());
                        }
                    }
                    WRITE_FD.store(-1, Ordering::Release);
                    return Err(error);
                }
            }
            Ok(Self {
                read,
                _write: write,
                old,
                restored: false,
            })
        }
        pub(super) fn reader(&self) -> io::Result<File> {
            self.read.try_clone()
        }
        pub(super) fn restore(&mut self) {
            if self.restored {
                return;
            }
            for (index, signal) in [libc::SIGINT, libc::SIGTERM].into_iter().enumerate() {
                unsafe {
                    libc::sigaction(signal, &self.old[index], std::ptr::null_mut());
                }
            }
            WRITE_FD.store(-1, Ordering::Release);
            self.restored = true;
        }
    }
    impl Drop for Listener {
        fn drop(&mut self) {
            self.restore();
        }
    }
    pub(super) fn wait(reader: &mut File) {
        // Block until the first notification. Afterwards retry pending intent
        // in case its wake arrived while the verdict lock was busy; force exit
        // never depends on that lock. Every handler run writes the pipe after
        // publishing FIRST, so a notification racing this load still wakes.
        let timeout = if FIRST.load(Ordering::Acquire) == 0 {
            -1
        } else {
            5
        };
        let mut fd = libc::pollfd {
            fd: reader.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        // SAFETY: one valid pollfd for a descriptor owned by `reader`.
        if unsafe { libc::poll(&mut fd, 1, timeout) } > 0 && fd.revents & libc::POLLIN != 0 {
            let _ = reader.read(&mut [0_u8; 64]);
        }
    }
}

#[cfg(windows)]
mod platform {
    use super::*;
    use std::sync::OnceLock;
    use std::time::{Duration, Instant};
    use windows_sys::Win32::System::Console::{
        CTRL_BREAK_EVENT, CTRL_C_EVENT, CTRL_CLOSE_EVENT, SetConsoleCtrlHandler,
    };
    static START: OnceLock<Instant> = OnceLock::new();
    unsafe extern "system" fn handler(event: u32) -> i32 {
        if matches!(event, CTRL_C_EVENT | CTRL_BREAK_EVENT | CTRL_CLOSE_EVENT) {
            notification(1);
            1
        } else {
            0
        }
    }
    pub(super) fn millis() -> u64 {
        START.get_or_init(Instant::now).elapsed().as_millis() as u64 + 1
    }
    pub(super) fn wake() {}
    #[derive(Debug)]
    pub(super) struct Listener {
        restored: bool,
    }
    impl Listener {
        pub(super) fn install() -> io::Result<Self> {
            START.get_or_init(Instant::now);
            if unsafe { SetConsoleCtrlHandler(Some(handler), 1) } == 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(Self { restored: false })
        }
        pub(super) fn reader(&self) -> io::Result<Reader> {
            Ok(Reader)
        }
        pub(super) fn restore(&mut self) {
            if !self.restored {
                unsafe {
                    SetConsoleCtrlHandler(Some(handler), 0);
                }
                self.restored = true;
            }
        }
    }
    impl Drop for Listener {
        fn drop(&mut self) {
            self.restore();
        }
    }
    #[derive(Debug)]
    pub(super) struct Reader;
    pub(super) fn wait(_: &mut Reader) {
        std::thread::sleep(Duration::from_millis(5));
    }
}
