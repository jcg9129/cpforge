// Bounded real filesystem fixture shared by capture and persistence tests.
#![allow(dead_code)]
use std::fs;
use std::io::Write;
#[cfg(target_os = "linux")]
use std::path::Path;
use std::path::PathBuf;
use std::process::Command;
use std::process::Stdio;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

const MAX_BYTES: u64 = 64 * 1024 * 1024;

#[derive(Debug)]
pub struct TinyFs {
    pub directory: PathBuf,
    mounted: bool,
    #[cfg(target_os = "linux")]
    sudo: bool,
    #[cfg(target_os = "macos")]
    image: PathBuf,
    #[cfg(target_os = "macos")]
    attach_attempted: bool,
}

fn bounded_command(command: &mut Command) -> std::io::Result<bool> {
    command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let mut child = command.spawn()?;
    let deadline =
        Instant::now() + Duration::from_secs(if cfg!(target_os = "macos") { 40 } else { 3 });
    let failure = loop {
        match child.try_wait() {
            Ok(Some(_)) => break None,
            // Polls the helper's exit; nothing else is coordinated here.
            Ok(None) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(1));
            }
            Ok(None) => {
                break Some(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    "mount helper timed out",
                ));
            }
            Err(error) => break Some(error),
        }
    };
    if failure.is_some() {
        let _ = child.kill();
    }
    // Every path reaps here; after a successful try_wait std returns the
    // cached status.
    let status = child.wait();
    match failure {
        Some(error) => Err(error),
        None => status.map(|status| status.success()),
    }
}

#[cfg(target_os = "linux")]
fn small_tmpfs(path: &Path) -> bool {
    use std::os::unix::ffi::OsStrExt;
    let Ok(path) = std::ffi::CString::new(path.as_os_str().as_bytes()) else {
        return false;
    };
    let mut stat = std::mem::MaybeUninit::<libc::statfs>::uninit();
    // SAFETY: path is NUL-terminated; stat has space for the full reply.
    if unsafe { libc::statfs(path.as_ptr(), stat.as_mut_ptr()) } != 0 {
        return false;
    }
    // SAFETY: statfs initialized the complete structure on success.
    let stat = unsafe { stat.assume_init() };
    stat.f_type == libc::TMPFS_MAGIC
        && stat.f_bsize > 0
        && stat.f_blocks.saturating_mul(stat.f_bsize as u64) <= MAX_BYTES
        && stat.f_bavail.saturating_mul(stat.f_bsize as u64) >= 128 * 1024
}

impl TinyFs {
    #[cfg(target_os = "linux")]
    pub fn acquire() -> Option<Self> {
        let name = format!("cpf-enospc-{}", std::process::id());
        let candidates = std::env::var_os("CPF_TEST_TINY_FS")
            .map(PathBuf::from)
            .into_iter()
            .chain([PathBuf::from("/dev/shm")]);
        for base in candidates {
            if small_tmpfs(&base) {
                let directory = base.join(&name);
                if fs::create_dir(&directory).is_ok() {
                    return Some(Self {
                        directory,
                        mounted: false,
                        sudo: false,
                    });
                }
            }
        }
        Self::one_mib()
    }

    #[cfg(target_os = "linux")]
    pub fn one_mib() -> Option<Self> {
        // Unique per process and call: test binaries and their threads may
        // each hold a mount at the same time.
        static NEXT: AtomicU32 = AtomicU32::new(0);
        let name = format!(
            "cpf-enospc-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        );
        let directory = std::env::current_dir().unwrap().join("target").join(name);
        fs::create_dir_all(directory.parent().unwrap()).unwrap();
        fs::create_dir(&directory).unwrap();
        // SAFETY: these identity queries have no pointer arguments or side effects.
        let (uid, gid) = unsafe { (libc::geteuid(), libc::getegid()) };
        let sudo = uid != 0;
        let mut command = if sudo {
            let mut command = Command::new("sudo");
            command.args(["-n", "mount"]);
            command
        } else {
            Command::new("mount")
        };
        command
            .args(["-t", "tmpfs", "-o"])
            .arg(format!("size=1m,mode=0700,uid={uid},gid={gid}"))
            .arg("tmpfs")
            .arg(&directory);
        let result = bounded_command(&mut command);
        if matches!(result, Ok(true)) {
            Some(Self {
                directory,
                mounted: true,
                sudo,
            })
        } else {
            fs::remove_dir(&directory).unwrap();
            assert!(
                std::env::var_os("CPF_REQUIRE_TMPFS").as_deref() != Some(std::ffi::OsStr::new("1")),
                "required real ENOSPC fixture: 1 MiB tmpfs mount unavailable: {result:?}"
            );
            eprintln!(
                "SKIP real ENOSPC: 1 MiB tmpfs mount via root/sudo -n unavailable: {result:?}"
            );
            None
        }
    }

    #[cfg(target_os = "macos")]
    pub fn acquire() -> Option<Self> {
        Self::one_mib()
    }

    // HFS+ needs room for its filesystem metadata. A 16 MiB writable image
    // stays below MAX_BYTES and exercises native ENOSPC without root or sudo.
    // Keep the shared entry point's name for Linux's 1 MiB tmpfs callers.
    #[cfg(target_os = "macos")]
    pub fn one_mib() -> Option<Self> {
        static NEXT: AtomicU32 = AtomicU32::new(0);
        let name = format!(
            "cpf-enospc-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        );
        let directory = std::env::current_dir().unwrap().join("target").join(name);
        let image = directory.with_extension("dmg");
        fs::create_dir_all(&directory).unwrap();
        let mut fixture = Self {
            directory,
            mounted: false,
            image,
            attach_attempted: false,
        };
        // Hosted runners occasionally report a transient "Resource busy" from
        // hdiutil; retry a prompt failure, but never a timed-out helper.
        let mut created = Ok(false);
        for _ in 0..3 {
            let _ = fs::remove_file(&fixture.image);
            created = bounded_command(
                Command::new("hdiutil")
                    .args([
                        "create",
                        "-quiet",
                        "-size",
                        "16m",
                        "-fs",
                        "HFS+",
                        "-layout",
                        "NONE",
                        "-volname",
                        "cpf-enospc",
                    ])
                    .arg(&fixture.image),
            );
            if !matches!(created, Ok(false)) {
                break;
            }
        }
        let mut attached = Ok(false);
        if matches!(created, Ok(true)) {
            // Mark before attach so even a timed-out/partially successful attach
            // gets a detach attempt from Drop.
            fixture.attach_attempted = true;
            for _ in 0..3 {
                attached = bounded_command(
                    Command::new("hdiutil")
                        .args([
                            "attach",
                            "-quiet",
                            "-nobrowse",
                            "-noautoopen",
                            "-mountpoint",
                        ])
                        .arg(&fixture.directory)
                        .arg(&fixture.image),
                );
                if !matches!(attached, Ok(false)) {
                    break;
                }
            }
        }
        if matches!(attached, Ok(true)) {
            fixture.mounted = true;
            return Some(fixture);
        }
        // A failed attach might not have mounted anything. Drop still tries to
        // detach the mountpoint before removing its local files.
        assert!(
            std::env::var_os("CPF_REQUIRE_TMPFS").as_deref() != Some(std::ffi::OsStr::new("1")),
            "required real ENOSPC fixture: HFS+ image unavailable: create={created:?}, attach={attached:?}"
        );
        eprintln!(
            "SKIP real ENOSPC: hdiutil image unavailable: create={created:?}, attach={attached:?}"
        );
        None
    }

    pub fn fill(&self) {
        self.fill_leaving(64 * 1024);
    }

    pub fn fill_completely(&self) {
        self.fill_leaving(0);
    }

    fn fill_leaving(&self, headroom: u64) {
        let path = self.directory.join("filler");
        let mut file = fs::File::create(path).unwrap();
        let bytes = [0_u8; 64 * 1024];
        let mut written = 0;
        // Some filesystems (HFS+) refuse a write that does not fit instead of
        // writing a short prefix, so halve the chunk until one byte no longer fits.
        let mut chunk = bytes.len();
        loop {
            match file.write(&bytes[..chunk]) {
                Ok(0) => panic!("tmpfs filler made no progress"),
                Ok(n) => {
                    written += n as u64;
                    assert!(written <= MAX_BYTES, "tmpfs grew beyond the test's bound");
                }
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(error) => {
                    assert_eq!(error.raw_os_error(), Some(libc::ENOSPC));
                    if chunk > 1 {
                        chunk /= 2;
                        continue;
                    }
                    // Capture tests keep a stored prefix; persistence tests
                    // leave zero headroom and fail the next nonempty write.
                    assert!(written >= 64 * 1024);
                    file.set_len(written - headroom).unwrap();
                    break;
                }
            }
        }
    }
}

impl Drop for TinyFs {
    fn drop(&mut self) {
        if let Ok(entries) = fs::read_dir(&self.directory) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    let _ = fs::remove_dir_all(path);
                } else {
                    let _ = fs::remove_file(path);
                }
            }
        }
        #[cfg(target_os = "linux")]
        if self.mounted {
            let mut command = if self.sudo {
                let mut command = Command::new("sudo");
                command.args(["-n", "umount"]);
                command
            } else {
                Command::new("umount")
            };
            let mut result = bounded_command(command.arg(&self.directory));
            if !matches!(result, Ok(true)) {
                // A worker that outlived a failed assertion can keep the mount
                // busy. Detach it lazily rather than leak it for later runs.
                let mut lazy = if self.sudo {
                    let mut command = Command::new("sudo");
                    command.args(["-n", "umount", "-l"]);
                    command
                } else {
                    let mut command = Command::new("umount");
                    command.arg("-l");
                    command
                };
                eprintln!("tmpfs unmount failed ({result:?}); detaching lazily");
                result = bounded_command(lazy.arg(&self.directory));
            }
            if !matches!(result, Ok(true)) {
                // Never panic while unwinding from a failed assertion.
                eprintln!("tmpfs cleanup failed: {result:?}");
                assert!(std::thread::panicking(), "tmpfs cleanup failed");
                return;
            }
        }
        #[cfg(target_os = "macos")]
        {
            if self.attach_attempted {
                let result = bounded_command(
                    Command::new("hdiutil")
                        .args(["detach", "-quiet", "-force"])
                        .arg(&self.directory),
                );
                // A failed attach need not have installed a device. A successful
                // attach must detach before its backing file can be removed.
                if self.mounted && !matches!(result, Ok(true)) {
                    eprintln!("image detach failed: {result:?}");
                    assert!(std::thread::panicking(), "image detach failed");
                    return;
                }
            }
            let _ = fs::remove_file(&self.image);
        }
        let removed = fs::remove_dir(&self.directory);
        // Disk Arbitration may remove a mount point itself on detach.
        let removed = match removed {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            other => other,
        };
        assert!(removed.is_ok() || std::thread::panicking(), "{removed:?}");
    }
}
