//! Invocation startup and panic-output bookkeeping. Initialize before file I/O.
use std::io;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

static STDOUT_CLOSED: AtomicBool = AtomicBool::new(false);
static OUTPUT_BEGUN: AtomicBool = AtomicBool::new(false);
static DELIVERY_FAILED: AtomicBool = AtomicBool::new(false);
static REPORT_FROZEN: AtomicBool = AtomicBool::new(false);

pub fn stdout_closed() -> bool {
    STDOUT_CLOSED.load(Ordering::Acquire)
}
pub fn output_begun() -> bool {
    OUTPUT_BEGUN.load(Ordering::Acquire)
}
pub fn delivery_failed() -> bool {
    DELIVERY_FAILED.load(Ordering::Acquire)
}
pub(crate) fn fail_delivery() {
    DELIVERY_FAILED.store(true, Ordering::Release);
}
pub fn report_frozen() -> bool {
    REPORT_FROZEN.load(Ordering::Acquire)
}
pub(crate) fn begin_output() {
    OUTPUT_BEGUN.store(true, Ordering::Release);
}
pub(crate) fn freeze_report() {
    REPORT_FROZEN.store(true, Ordering::Release);
}

/// Records a closed stdout before the Rust runtime starts. The runtime reopens
/// closed standard descriptors to `/dev/null` before `main`, so the binary
/// registers this function as a process constructor; `initialize` covers
/// embedders and platforms without one.
#[cfg(unix)]
pub extern "C" fn probe_standard_streams() {
    // SAFETY: F_GETFD only queries descriptor 1; no state is changed.
    if unsafe { libc::fcntl(1, libc::F_GETFD) } == -1 {
        STDOUT_CLOSED.store(true, Ordering::Release);
    }
}

/// Startup rules for standard streams and `SIGPIPE`. Call first in `main`,
/// before parsing arguments, opening files or starting threads.
pub fn initialize() -> io::Result<()> {
    #[cfg(unix)]
    unsafe {
        // SAFETY: startup is single threaded. Repair standard fds before any
        // library can reuse their numbers for an unrelated file.
        for fd in 0..=2 {
            if libc::fcntl(fd, libc::F_GETFD) >= 0 {
                continue;
            }
            let error = io::Error::last_os_error();
            if error.raw_os_error() != Some(libc::EBADF) {
                return Err(error);
            }
            if fd == 1 {
                STDOUT_CLOSED.store(true, Ordering::Release);
            }
            let opened = libc::open(c"/dev/null".as_ptr(), libc::O_RDWR);
            if opened < 0 {
                return Err(io::Error::last_os_error());
            }
            if opened != fd {
                let result = libc::dup2(opened, fd);
                libc::close(opened);
                if result < 0 {
                    return Err(io::Error::last_os_error());
                }
            }
        }
        if libc::signal(libc::SIGPIPE, libc::SIG_IGN) == libc::SIG_ERR {
            return Err(io::Error::last_os_error());
        }
    }
    #[cfg(windows)]
    {
        use windows_sys::Win32::{
            Foundation::INVALID_HANDLE_VALUE,
            System::Console::{GetStdHandle, STD_OUTPUT_HANDLE},
        };
        // SAFETY: querying an inherited standard handle does not transfer it.
        let handle = unsafe { GetStdHandle(STD_OUTPUT_HANDLE) };
        STDOUT_CLOSED.store(
            handle.is_null() || handle == INVALID_HANDLE_VALUE,
            Ordering::Release,
        );
    }
    Ok(())
}

// Armed before admission by MachineArgs::prepare. The top-level panic boundary
// can then choose a document without inspecting possibly half-built job state.
static MACHINE: Mutex<Option<(crate::transport::MachineMode, crate::report::JobKind)>> =
    Mutex::new(None);
pub(crate) fn arm_machine(mode: crate::transport::MachineMode, command: crate::report::JobKind) {
    if mode.requested() {
        *MACHINE.lock().unwrap_or_else(|e| e.into_inner()) = Some((mode, command));
    }
}
pub fn escaped_panic() {
    let context = *MACHINE.lock().unwrap_or_else(|e| e.into_inner());
    if let Some((mode, command)) = context {
        let _ = crate::transport::Delivery::stdout().escaped_job_panic(command, mode);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn panic_helper() {
        let Ok(mode) = std::env::var("CPF_PANIC_HELPER") else {
            return;
        };
        initialize().unwrap();
        std::panic::set_hook(Box::new(|_| {}));
        arm_machine(
            crate::transport::MachineMode::Json,
            crate::report::JobKind::Test,
        );
        if mode == "frozen" {
            freeze_report();
        }
        if mode == "failed" {
            fail_delivery();
        }
        if mode == "begun" {
            crate::transport::Delivery::stdout()
                .document(b"{}")
                .unwrap();
        }
        assert!(std::panic::catch_unwind(|| panic!("injected panic")).is_err());
        if mode == "nonjob" {
            crate::transport::Delivery::stdout()
                .escaped_panic("current")
                .unwrap();
        } else {
            escaped_panic();
        }
        std::process::exit(70);
    }
    #[test]
    fn panic_documents_respect_output_and_freeze_boundaries() {
        for mode in ["before", "frozen", "begun", "nonjob", "failed"] {
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "process::tests::panic_helper", "--nocapture"])
                .env("CPF_PANIC_HELPER", mode)
                .output()
                .unwrap();
            assert_eq!(output.status.code(), Some(70), "{output:?}");
            let bytes = String::from_utf8(output.stdout).unwrap();
            let documents: Vec<serde_json::Value> = bytes
                .lines()
                .filter_map(|line| {
                    line.find('{')
                        .and_then(|start| serde_json::from_str(&line[start..]).ok())
                })
                .collect();
            match mode {
                "before" => {
                    assert_eq!(documents.len(), 1, "{bytes}");
                    assert_eq!(documents[0]["outcome"], "refused");
                    assert_eq!(documents[0]["refusal"]["class"], "internal");
                }
                "nonjob" => {
                    assert_eq!(documents.len(), 1, "{bytes}");
                    assert_eq!(documents[0]["document"], "error");
                }
                "frozen" | "failed" => assert!(documents.is_empty(), "{bytes}"),
                "begun" => assert_eq!(documents, [serde_json::json!({})]),
                _ => unreachable!(),
            }
        }
    }
    #[cfg(unix)]
    #[test]
    fn closed_descriptors_helper() {
        use std::os::fd::AsRawFd;
        if std::env::var_os("CPF_CLOSED_FDS_HELPER").is_none() {
            return;
        }
        // SAFETY: isolated subprocess, no other threads or open files rely on
        // these standard descriptors. initialize repairs them before any open.
        unsafe {
            for fd in 0..=2 {
                libc::close(fd);
            }
        }
        initialize().unwrap();
        for fd in 0..=2 {
            assert!(unsafe { libc::fcntl(fd, libc::F_GETFD) } >= 0);
        }
        assert!(stdout_closed());
        assert_eq!(
            crate::transport::Delivery::stdout()
                .preflight()
                .unwrap_err()
                .exit_code(),
            crate::exit::ExitCode::Delivery
        );
        let file = std::fs::File::open(concat!(env!("CARGO_MANIFEST_DIR"), "/Cargo.toml")).unwrap();
        assert!(file.as_raw_fd() > 2);
        std::process::exit(0);
    }
    #[cfg(unix)]
    #[test]
    fn closed_standard_descriptors_are_repaired_before_opening_files() {
        let status = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "process::tests::closed_descriptors_helper",
                "--nocapture",
            ])
            .env("CPF_CLOSED_FDS_HELPER", "1")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped())
            .status()
            .unwrap();
        assert!(status.success(), "{status:?}");
    }
}
