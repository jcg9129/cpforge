//! Native filesystem probes: portable keys deliberately over-flag host aliases.

use std::fs::{self, OpenOptions};
use std::io;
use std::path::PathBuf;
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use cpforge::names::collision_key;

#[derive(Debug)]
struct ProbeDirectory(PathBuf);

impl ProbeDirectory {
    fn new() -> io::Result<Self> {
        let temp = std::env::temp_dir();
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        for attempt in 0..128 {
            let path = temp.join(format!(
                "cpforge-names-{}-{stamp}-{attempt}",
                std::process::id()
            ));
            match fs::create_dir(&path) {
                Ok(()) => return Ok(Self(path)),
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(error),
            }
        }
        Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "could not allocate probe directory",
        ))
    }

    fn record_filesystem(&self) {
        println!(
            "native name probe: OS={}, directory={:?}",
            std::env::consts::OS,
            self.0
        );
        // Diagnostics only: CI can lack these utilities or filesystem queries.
        #[cfg(unix)]
        let output = {
            let mut command = Command::new("df");
            #[cfg(target_os = "linux")]
            command.arg("-T");
            command.arg(&self.0).output()
        };
        #[cfg(windows)]
        let output = Command::new("powershell.exe")
            .args(["-NoProfile", "-NonInteractive", "-Command",
                "& { param($path) Get-Volume -FilePath $path | Format-List DriveLetter,FileSystem,FileSystemLabel }"])
            .arg(&self.0)
            .output();
        #[cfg(any(unix, windows))]
        match output {
            Ok(output) => {
                println!("filesystem query status: {}", output.status);
                // Write diagnostic bytes directly, without using them as name data.
                use std::io::Write;
                let _ = io::stdout().write_all(&output.stdout);
                let _ = io::stdout().write_all(&output.stderr);
            }
            Err(error) => println!("filesystem query unavailable: {error}"),
        }
    }

    fn probe(&self, first: &str, second: &str) -> io::Result<()> {
        assert_eq!(collision_key(first), collision_key(second));
        drop(
            OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(self.0.join(first))?,
        );
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(self.0.join(second))
        {
            Ok(file) => {
                drop(file);
                println!("{first:?} then {second:?}: host refused second name=false");
            }
            Err(error) => {
                println!("{first:?} then {second:?}: host refused second name=true ({error})");
                if error.kind() != io::ErrorKind::AlreadyExists {
                    return Err(error);
                }
            }
        }
        Ok(())
    }
}

impl Drop for ProbeDirectory {
    fn drop(&mut self) {
        match fs::remove_dir_all(&self.0) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => eprintln!("could not clean up native name probe {:?}: {error}", self.0),
        }
    }
}

#[test]
fn portable_keys_flag_case_and_normalization_pairs_on_the_native_filesystem() -> io::Result<()> {
    let directory = ProbeDirectory::new()?;
    directory.record_filesystem();
    directory.probe("a.txt", "A.txt")?;
    directory.probe("é", "e\u{0301}")?;
    // Assert cleanup on success; Drop also attempts it if the test fails early.
    fs::remove_dir_all(&directory.0)?;
    Ok(())
}
