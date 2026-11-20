//! Workspace initialization through the real command-line binary.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use cpforge::storage::{Record, job_seq::classify_job_sequence};

#[derive(Debug)]
struct Directory(PathBuf);

impl Directory {
    fn new() -> Self {
        let mut bits = [0_u8; 16];
        getrandom::fill(&mut bits).unwrap();
        // TMPDIR may itself point inside a checkout. Choose a writable
        // ancestor outside every git tree so host git state cannot leak in.
        let temporary = fs::canonicalize(std::env::temp_dir()).unwrap();
        let path = temporary
            .ancestors()
            .find_map(|base| {
                if base
                    .ancestors()
                    .any(|ancestor| fs::symlink_metadata(ancestor.join(".git")).is_ok())
                {
                    return None;
                }
                let candidate = base.join(format!(
                    "cpforge-init-c4c-{:032x}",
                    u128::from_le_bytes(bits)
                ));
                fs::create_dir(&candidate).ok().map(|()| candidate)
            })
            .expect("a writable temporary directory outside a git tree");
        Self(fs::canonicalize(path).unwrap())
    }

    fn path(&self, relative: impl AsRef<Path>) -> PathBuf {
        self.0.join(relative)
    }
}

impl Drop for Directory {
    fn drop(&mut self) {
        // Fixture teardown only; command recovery uses the no-follow walker.
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn command(root: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_cpf"));
    command.current_dir(root).env_remove("CPF_STORAGE_CRASH_AT");
    command
}

fn init(root: &Path) -> Output {
    command(root).arg("init").arg(root).output().unwrap()
}

fn assert_success(output: &Output) {
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    assert!(output.stderr.is_empty(), "{output:?}");
}

fn assert_complete(root: &Path) -> Vec<u8> {
    for relative in [".cpforge", ".cpforge/runs", ".cpforge/staging"] {
        assert!(root.join(relative).is_dir(), "{relative}");
    }
    assert!(root.join(".cpforge/lock").is_file());
    assert_eq!(
        fs::read(root.join("cpforge.toml")).unwrap(),
        b"schema = 1\n"
    );
    let bytes = fs::read(root.join(".cpforge/job-seq")).unwrap();
    let Record::Valid(sequence) = classify_job_sequence(&bytes) else {
        panic!("invalid sequence: {bytes:?}");
    };
    assert_eq!(sequence.epoch.len(), 32);
    assert!(
        sequence
            .epoch
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    );
    assert_eq!(sequence.next, 0);
    let document: toml_edit::DocumentMut = std::str::from_utf8(&bytes).unwrap().parse().unwrap();
    assert_eq!(document["schema"].as_integer(), Some(1));
    assert_eq!(document["next"].as_str(), Some("0"));
    bytes
}

#[test]
fn fresh_init_and_default_path_are_idempotent() {
    let dir = Directory::new();
    let first = command(&dir.0).arg("init").output().unwrap();
    assert_success(&first);
    assert!(String::from_utf8_lossy(&first.stdout).contains("created cpforge.toml"));
    let sequence = assert_complete(&dir.0);
    let config = fs::read(dir.path("cpforge.toml")).unwrap();
    let second = init(&dir.0);
    assert_success(&second);
    assert!(String::from_utf8_lossy(&second.stdout).contains("already initialised"));
    assert_eq!(fs::read(dir.path("cpforge.toml")).unwrap(), config);
    assert_eq!(assert_complete(&dir.0), sequence);
    let mut entries: Vec<_> = fs::read_dir(&dir.0)
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect();
    entries.sort();
    assert_eq!(
        entries,
        [
            std::ffi::OsString::from(".cpforge"),
            std::ffi::OsString::from("cpforge.toml")
        ]
    );
    let mut entries: Vec<_> = fs::read_dir(dir.path(".cpforge"))
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect();
    entries.sort();
    assert_eq!(
        entries,
        ["job-seq", "lock", "runs", "staging"].map(std::ffi::OsString::from)
    );
}

#[test]
fn existing_config_never_causes_sequence_creation_or_repair() {
    for sequence in [None, Some(b"schema = 1\nnext = '01'\n".as_slice())] {
        let dir = Directory::new();
        let config = b"# user configuration\nschema = 99\ncustom = 'unchanged'\n";
        fs::write(dir.path("cpforge.toml"), config).unwrap();
        if let Some(bytes) = sequence {
            fs::create_dir(dir.path(".cpforge")).unwrap();
            fs::write(dir.path(".cpforge/job-seq"), bytes).unwrap();
        }
        let output = init(&dir.0);
        assert_success(&output);
        assert!(String::from_utf8_lossy(&output.stdout).contains("already initialised"));
        assert!(!String::from_utf8_lossy(&output.stdout).contains("job-seq"));
        assert_eq!(fs::read(dir.path("cpforge.toml")).unwrap(), config);
        assert!(dir.path(".cpforge/runs").is_dir());
        assert!(dir.path(".cpforge/staging").is_dir());
        match sequence {
            Some(bytes) => assert_eq!(fs::read(dir.path(".cpforge/job-seq")).unwrap(), bytes),
            None => assert!(!dir.path(".cpforge/job-seq").exists()),
        }
    }
}

#[test]
fn interrupted_initial_sequence_is_kept_byte_for_byte() {
    let dir = Directory::new();
    fs::create_dir(dir.path(".cpforge")).unwrap();
    let bytes = b"# retained epoch and formatting\nschema=1\nepoch='0123456789abcdef0123456789abcdef'\nnext='18446744073709551615'\n";
    fs::write(dir.path(".cpforge/job-seq"), bytes).unwrap();
    let output = init(&dir.0);
    assert_success(&output);
    assert!(String::from_utf8_lossy(&output.stdout).contains("kept .cpforge/job-seq"));
    assert_eq!(fs::read(dir.path(".cpforge/job-seq")).unwrap(), bytes);
    assert_eq!(fs::read(dir.path("cpforge.toml")).unwrap(), b"schema = 1\n");
}

#[test]
fn unusable_initial_sequences_refuse_without_changing_the_record() {
    for (bytes, classification) in [
        (
            b"schema = 1\nepoch = '0123456789abcdef0123456789abcdef'\nnext = '01'\n".as_slice(),
            "malformed",
        ),
        (
            b"schema = 1\nepoch = '0123456789abcdef0123456789abcdef'\nnext = 5\n".as_slice(),
            "malformed",
        ),
        (
            b"schema = 1\nepoch = '0123'\nnext = '0'\n".as_slice(),
            "malformed",
        ),
        (
            b"epoch = '0123456789abcdef0123456789abcdef'\nnext = '0'\n".as_slice(),
            "malformed",
        ),
        (b"schema = 99\nnext = 5\n".as_slice(), "too_new"),
    ] {
        let dir = Directory::new();
        fs::create_dir(dir.path(".cpforge")).unwrap();
        let path = dir.path(".cpforge/job-seq");
        fs::write(&path, bytes).unwrap();
        let output = init(&dir.0);
        assert_eq!(output.status.code(), Some(1), "{output:?}");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stderr.starts_with("cpf: error: "), "{stderr}");
        assert!(stderr.contains(classification), "{stderr}");
        assert!(stderr.contains(&path.display().to_string()), "{stderr}");
        assert!(!dir.path("cpforge.toml").exists());
        assert_eq!(fs::read(path).unwrap(), bytes);
    }
}

#[test]
fn non_regular_initial_sequence_is_unreadable() {
    let dir = Directory::new();
    fs::create_dir_all(dir.path(".cpforge/job-seq")).unwrap();
    fs::write(dir.path(".cpforge/job-seq/keep"), b"untouched").unwrap();
    let output = init(&dir.0);
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    assert!(String::from_utf8_lossy(&output.stderr).contains("unreadable"));
    assert!(String::from_utf8_lossy(&output.stderr).contains("job-seq"));
    assert!(!dir.path("cpforge.toml").exists());
    assert_eq!(
        fs::read(dir.path(".cpforge/job-seq/keep")).unwrap(),
        b"untouched"
    );
}

#[cfg(unix)]
#[test]
fn unreadable_initial_sequence_is_preserved() {
    use std::os::unix::fs::PermissionsExt;
    // SAFETY: geteuid has no preconditions and changes no process state.
    if unsafe { libc::geteuid() } == 0 {
        return;
    }
    let dir = Directory::new();
    fs::create_dir(dir.path(".cpforge")).unwrap();
    let path = dir.path(".cpforge/job-seq");
    let bytes = b"schema = 1\nepoch = '0123456789abcdef0123456789abcdef'\nnext = '0'\n";
    fs::write(&path, bytes).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o000)).unwrap();
    let output = init(&dir.0);
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    assert!(String::from_utf8_lossy(&output.stderr).contains("unreadable"));
    assert!(String::from_utf8_lossy(&output.stderr).contains("job-seq"));
    assert!(!dir.path("cpforge.toml").exists());
    assert_eq!(fs::read(path).unwrap(), bytes);
}

#[test]
fn missing_root_is_not_created_and_extra_arguments_are_usage_errors() {
    let dir = Directory::new();
    let missing = dir.path("missing");
    let output = command(&dir.0).arg("init").arg(&missing).output().unwrap();
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    assert!(!missing.exists());
    let output = command(&dir.0)
        .args(["init", ".", "extra"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2), "{output:?}");
    assert!(!dir.path(".cpforge").exists());
}

fn link_directory(source: &Path, target: &Path) {
    #[cfg(unix)]
    std::os::unix::fs::symlink(source, target).unwrap();
    #[cfg(windows)]
    {
        // Junctions require no developer mode or symlink privilege.
        let ordinary = |path: &Path| {
            let path = path.as_os_str().to_string_lossy();
            if let Some(unc) = path.strip_prefix(r"\\?\UNC\") {
                PathBuf::from(format!(r"\\{unc}"))
            } else {
                PathBuf::from(path.strip_prefix(r"\\?\").unwrap_or(&path))
            }
        };
        let output = Command::new("cmd.exe")
            .args(["/C", "mklink", "/J"])
            .arg(ordinary(target))
            .arg(ordinary(source))
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
    }
}

#[test]
fn metadata_links_are_refused_and_root_links_work() {
    let dir = Directory::new();
    let outside = Directory::new();
    fs::write(outside.path("keep"), b"untouched").unwrap();
    link_directory(&outside.0, &dir.path(".cpforge"));
    let output = init(&dir.0);
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    assert!(String::from_utf8_lossy(&output.stderr).contains("refusing a link or junction"));
    assert!(!dir.path("cpforge.toml").exists());
    assert_eq!(fs::read_dir(&outside.0).unwrap().count(), 1);
    assert_eq!(fs::read(outside.path("keep")).unwrap(), b"untouched");

    let root = Directory::new();
    let aliases = Directory::new();
    let alias = aliases.path("root");
    link_directory(&root.0, &alias);
    let output = init(&alias);
    assert_success(&output);
    assert_complete(&root.0);
}

#[test]
fn structural_directory_links_are_refused() {
    for relative in [".cpforge/runs", ".cpforge/staging"] {
        let dir = Directory::new();
        let outside = Directory::new();
        fs::create_dir(dir.path(".cpforge")).unwrap();
        link_directory(&outside.0, &dir.path(relative));
        let output = init(&dir.0);
        assert_eq!(output.status.code(), Some(1), "{output:?}");
        assert!(!dir.path("cpforge.toml").exists());
        assert!(!dir.path(".cpforge/job-seq").exists());
        assert_eq!(fs::read_dir(&outside.0).unwrap().count(), 0);
    }
}

#[test]
fn git_advisory_accepts_git_files_and_directories_in_ancestors() {
    let plain = Directory::new();
    let output = init(&plain.0);
    assert_success(&output);
    assert!(!String::from_utf8_lossy(&output.stdout).contains("cpf gitignore"));
    for file in [false, true] {
        let dir = Directory::new();
        let git = dir.path(".git");
        if file {
            fs::write(&git, b"gitdir: elsewhere\n").unwrap();
        } else {
            fs::create_dir(&git).unwrap();
        }
        for root in [dir.0.clone(), dir.path("nested")] {
            if root != dir.0 {
                fs::create_dir(&root).unwrap();
            }
            let output = init(&root);
            assert_success(&output);
            assert!(String::from_utf8_lossy(&output.stdout).contains("cpf gitignore"));
            assert!(!root.join(".gitignore").exists());
        }
        if file {
            assert_eq!(fs::read(&git).unwrap(), b"gitdir: elsewhere\n");
        } else {
            assert_eq!(fs::read_dir(&git).unwrap().count(), 0);
        }
    }
}

#[cfg(not(feature = "storage-test-hooks"))]
#[test]
fn crash_environment_is_ignored_without_test_hooks() {
    let dir = Directory::new();
    let output = command(&dir.0)
        .arg("init")
        .env("CPF_STORAGE_CRASH_AT", "init.locked")
        .output()
        .unwrap();
    assert_success(&output);
    assert_complete(&dir.0);
}

#[test]
fn simultaneous_initializers_keep_one_complete_workspace() {
    let dir = Directory::new();
    let first = command(&dir.0)
        .arg("init")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let second = init(&dir.0);
    assert_success(&second);
    let sequence = assert_complete(&dir.0);
    let first = first.wait_with_output().unwrap();
    assert_success(&first);
    assert_eq!(assert_complete(&dir.0), sequence);
    let first_kept = String::from_utf8_lossy(&first.stdout).contains("already initialised");
    let second_kept = String::from_utf8_lossy(&second.stdout).contains("already initialised");
    assert_ne!(first_kept, second_kept);
}

#[cfg(feature = "storage-test-hooks")]
mod crash_tests {
    use super::*;

    fn assert_aborted(output: &Output, selected: &str) {
        let point = selected.split('#').next().unwrap();
        let marker = format!("CPF-CRASH-AT {point}");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.lines().any(|line| line == marker),
            "{selected}: {output:?}"
        );
        assert!(!output.status.success(), "{output:?}");
        #[cfg(unix)]
        {
            use std::os::unix::process::ExitStatusExt;
            assert_eq!(output.status.signal(), Some(libc::SIGABRT), "{output:?}");
        }
        #[cfg(windows)]
        {
            let code = output.status.code().expect("Windows process exit code") as u32;
            assert!(
                code == 3 || code == 0x4000_0015 || code >= 0x8000_0000,
                "{output:?}"
            );
        }
    }

    fn aborting_init(root: &Path, point: &str) -> Output {
        let mut command = command(root);
        command
            .arg("init")
            .arg(root)
            .env("CPF_STORAGE_CRASH_AT", point);
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            // SAFETY: the child hook calls only setrlimit and constructs an OS
            // error on failure; it takes no locks or allocating paths.
            unsafe {
                command.pre_exec(|| {
                    let limit = libc::rlimit {
                        rlim_cur: 0,
                        rlim_max: 0,
                    };
                    if libc::setrlimit(libc::RLIMIT_CORE, &limit) == 0 {
                        Ok(())
                    } else {
                        Err(std::io::Error::last_os_error())
                    }
                });
            }
        }
        #[cfg(windows)]
        {
            static ERROR_MODE: std::sync::Once = std::sync::Once::new();
            ERROR_MODE.call_once(|| {
                // SAFETY: children inherit this test process's error mode;
                // suppress crash dialogs for unattended command-abort tests.
                unsafe {
                    windows_sys::Win32::System::Diagnostics::Debug::SetErrorMode(
                        windows_sys::Win32::System::Diagnostics::Debug::SEM_NOGPFAULTERRORBOX,
                    );
                }
            });
        }
        command.output().unwrap()
    }

    fn assert_no_temps(root: &Path) {
        for directory in [root.to_path_buf(), root.join(".cpforge")] {
            assert!(fs::read_dir(directory).unwrap().all(|entry| {
                !entry
                    .unwrap()
                    .file_name()
                    .to_string_lossy()
                    .starts_with(".cpf-tmp-")
            }));
        }
    }

    #[test]
    fn every_init_and_publication_boundary_recovers_the_allowed_state() {
        let mut points = vec![
            ("lock.acquired", false, false),
            ("lock.recovered", false, false),
            ("init.locked", false, false),
            ("file_noclobber.before_publish", false, false),
            ("file_noclobber.after_publish", true, false),
            ("file_noclobber.directory_synced", true, false),
            ("init.job_seq_published", true, false),
            ("file_noclobber.before_publish#2", true, false),
            ("file_noclobber.after_publish#2", true, true),
            ("file_noclobber.directory_synced#2", true, true),
            ("init.config_published", true, true),
        ];
        if cfg!(target_os = "linux") {
            points.extend([
                ("file_noclobber.linked", true, false),
                ("file_noclobber.linked#2", true, true),
            ]);
        }
        for (point, sequence_present, config_present) in points {
            let dir = Directory::new();
            fs::create_dir_all(dir.path(".cpforge/staging/old")).unwrap();
            fs::write(dir.path(".cpforge/staging/old/data"), b"leftover").unwrap();
            let root_temp = dir.path(format!(".cpf-tmp-{}", "a".repeat(32)));
            let metadata_temp = dir.path(format!(".cpforge/.cpf-tmp-{}", "b".repeat(32)));
            fs::write(&root_temp, b"leftover").unwrap();
            fs::write(&metadata_temp, b"leftover").unwrap();

            let output = aborting_init(&dir.0, point);
            assert_aborted(&output, point);
            assert!(dir.path(".cpforge/lock").is_file());
            for leftover in [
                &root_temp,
                &metadata_temp,
                &dir.path(".cpforge/staging/old"),
            ] {
                assert_eq!(leftover.exists(), point == "lock.acquired", "{point}");
            }
            assert_eq!(
                dir.path(".cpforge/job-seq").exists(),
                sequence_present,
                "{point}"
            );
            assert_eq!(dir.path("cpforge.toml").exists(), config_present, "{point}");
            if config_present {
                assert_eq!(fs::read(dir.path("cpforge.toml")).unwrap(), b"schema = 1\n");
            }
            let sequence =
                sequence_present.then(|| fs::read(dir.path(".cpforge/job-seq")).unwrap());
            if let Some(bytes) = &sequence {
                assert!(
                    matches!(classify_job_sequence(bytes), Record::Valid(sequence) if sequence.next == 0),
                    "{point}"
                );
            }
            if point.starts_with("file_noclobber.before_publish")
                || point.starts_with("file_noclobber.linked")
            {
                let parent = if point.ends_with("#2") {
                    dir.0.clone()
                } else {
                    dir.path(".cpforge")
                };
                assert!(
                    fs::read_dir(parent).unwrap().any(|entry| entry
                        .unwrap()
                        .file_name()
                        .to_string_lossy()
                        .starts_with(".cpf-tmp-")),
                    "{point}"
                );
            }

            // This actual command rerun proves recovery and OS-lock release
            // after process death, before creating any new publication temps.
            let output = init(&dir.0);
            assert_success(&output);
            let final_sequence = assert_complete(&dir.0);
            let stdout = String::from_utf8_lossy(&output.stdout);
            if config_present {
                assert!(stdout.contains("already initialised"), "{point}: {stdout}");
            } else {
                assert!(stdout.contains("created cpforge.toml"), "{point}: {stdout}");
                assert!(
                    stdout.contains(if sequence_present {
                        "kept .cpforge/job-seq"
                    } else {
                        "created .cpforge/job-seq"
                    }),
                    "{point}: {stdout}"
                );
            }
            if let Some(sequence) = sequence {
                assert_eq!(final_sequence, sequence, "{point}");
            }
            assert_no_temps(&dir.0);
            assert!(
                fs::read_dir(dir.path(".cpforge/staging"))
                    .unwrap()
                    .next()
                    .is_none()
            );
        }
    }
}

#[cfg(unix)]
#[test]
fn linked_config_on_an_existing_workspace_is_accepted() {
    let dir = Directory::new();
    let outside = Directory::new();
    fs::write(outside.path("cpforge.toml"), b"schema = 1\n").unwrap();
    std::os::unix::fs::symlink(outside.path("cpforge.toml"), dir.path("cpforge.toml")).unwrap();
    let output = init(&dir.0);
    assert_success(&output);
    // An existing workspace: the linked config is untouched and no job-seq is created.
    assert!(
        fs::symlink_metadata(dir.path("cpforge.toml"))
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert_eq!(
        fs::read(outside.path("cpforge.toml")).unwrap(),
        b"schema = 1\n"
    );
    assert!(!dir.path(".cpforge/job-seq").exists());
}

#[test]
fn malformed_initial_sequence_error_is_readable() {
    let dir = Directory::new();
    fs::create_dir(dir.path(".cpforge")).unwrap();
    fs::write(dir.path(".cpforge/job-seq"), b"schema = 1\nnext = '01'\n").unwrap();
    let output = init(&dir.0);
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("malformed: epoch must be"), "{stderr}");
    assert!(stderr.contains("rerun `cpf init`"), "{stderr}");
    assert!(
        !stderr.contains("Toml(") && !stderr.contains("Fields("),
        "{stderr}"
    );
    assert!(!dir.path("cpforge.toml").exists());
}

#[test]
fn directory_named_cpforge_toml_is_not_an_existing_workspace() {
    let dir = Directory::new();
    fs::create_dir(dir.path("cpforge.toml")).unwrap();
    let output = init(&dir.0);
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    assert!(String::from_utf8_lossy(&output.stderr).contains("not a regular workspace marker"));
    assert!(!dir.path(".cpforge/job-seq").exists());
}
