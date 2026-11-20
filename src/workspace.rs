//! Initial workspace creation under one storage guard.

use std::fmt::Write as _;
use std::fs;
use std::io;
use std::path::Path;

use crate::error::Error;
use crate::storage::{
    Cleanup, Durability, ErrorKind, Published, Record, StorageError, WorkspaceGuard, WorkspaceLock,
    crash, job_seq::read_job_sequence, publish_file_noclobber,
};

#[derive(Debug, Default)]
struct InitReport {
    lines: Vec<String>,
    warnings: Vec<String>,
}

/// Preserve progress and warnings even when a later step refuses to continue.
pub(crate) fn init(root: &Path) -> Result<(), Error> {
    let mut report = InitReport::default();
    let result = initialize(root, &mut report);
    for line in report.lines {
        println!("{line}");
    }
    for warning in report.warnings {
        eprintln!("cpf: warning: {warning}");
    }
    result
}

fn initialize(root: &Path, report: &mut InitReport) -> Result<(), Error> {
    let lock = WorkspaceLock::new(root)?;
    let guard = lock.acquire("init")?;
    initialize_locked(&guard, report)
}

fn initialize_locked(guard: &WorkspaceGuard<'_>, report: &mut InitReport) -> Result<(), Error> {
    if let Some(warning) = guard.recovery_warning() {
        for (path, error) in warning.paths.iter().zip(&warning.errors) {
            report.warnings.push(format!(
                "leftover recovery incomplete at {}: {error}",
                path.display()
            ));
        }
    }
    crash::point("init.locked");
    // The tool only checks for cpforge.toml here, so a linked config is allowed.
    let config = guard.entry_path(Path::new("cpforge.toml"))?;
    let initial = !exists(&config)?;
    report.lines.push(format!(
        "workspace lock acquired: {}",
        guard.root().join(".cpforge/lock").display()
    ));
    if guard
        .root()
        .ancestors()
        .any(|ancestor| fs::symlink_metadata(ancestor.join(".git")).is_ok())
    {
        report
            .lines
            .push("note: use `cpf gitignore` to review workspace ignore rules".to_owned());
    }
    for relative in [".cpforge/runs", ".cpforge/staging"] {
        let path = guard.check_path(relative)?;
        let kept = exists(&path)?;
        guard.create_dir_all(&path)?;
        report.lines.push(format!(
            "{} {relative}/",
            if kept { "kept" } else { "created" }
        ));
    }
    if !initial {
        report
            .lines
            .push("already initialised; kept cpforge.toml".to_owned());
        return Ok(());
    }

    ensure_initial_sequence(guard, report)?;
    match publish_file_noclobber(guard, &config, b"schema = 1\n") {
        Ok(published) => {
            publication_warnings(report, &config, &published);
            crash::point("init.config_published");
            report
                .lines
                .push("created cpforge.toml; workspace initialised".to_owned());
        }
        Err(error) => {
            cleanup_warning(report, &error.cleanup);
            if matches!(&error.kind, ErrorKind::Exists { .. }) {
                report
                    .lines
                    .push("already initialised; kept cpforge.toml".to_owned());
            } else {
                return Err(error.into());
            }
        }
    }
    Ok(())
}

fn ensure_initial_sequence(
    guard: &WorkspaceGuard<'_>,
    report: &mut InitReport,
) -> Result<(), Error> {
    let path = guard.check_path(".cpforge/job-seq")?;
    let mut record = read_job_sequence(&path);
    if matches!(&record, Record::Missing) {
        let mut random = [0_u8; 16];
        getrandom::fill(&mut random).map_err(|error| {
            Error::Failure(format!(
                "cannot generate epoch for {}: {error}",
                path.display()
            ))
        })?;
        let mut epoch = String::with_capacity(32);
        for byte in random {
            write!(epoch, "{byte:02x}").expect("String formatting");
        }
        let bytes = format!("schema = 1\nepoch = \"{epoch}\"\nnext = \"0\"\n");
        match publish_file_noclobber(guard, &path, bytes.as_bytes()) {
            Ok(published) => {
                publication_warnings(report, &path, &published);
                crash::point("init.job_seq_published");
                report.lines.push("created .cpforge/job-seq".to_owned());
                return Ok(());
            }
            Err(error) => {
                cleanup_warning(report, &error.cleanup);
                if !matches!(&error.kind, ErrorKind::Exists { .. }) {
                    return Err(error.into());
                }
                guard.check_path(&path)?;
                record = read_job_sequence(&path);
            }
        }
    }
    let reason = match record {
        Record::Valid(_) => {
            report.lines.push("kept .cpforge/job-seq".to_owned());
            return Ok(());
        }
        Record::Missing => "missing after concurrent creation".to_owned(),
        Record::Unreadable(error) => format!("unreadable: {error}"),
        Record::Malformed(error) => format!("malformed: {error}"),
        Record::TooNew { schema, supported } => {
            format!("too_new: schema {schema}, supported {supported}")
        }
    };
    Err(Error::Failure(format!(
        "{} is {reason}; cpforge.toml was not published (inspect it, or remove it if \
         no jobs of this workspace need their numbering kept, then rerun `cpf init`)",
        path.display()
    )))
}

fn exists(path: &Path) -> Result<bool, Error> {
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(StorageError::io(path, error).into()),
    }
}

fn publication_warnings(report: &mut InitReport, path: &Path, published: &Published) {
    if let Durability::Uncertain(error) = &published.durability {
        report.warnings.push(format!(
            "{} was published; durability uncertain: {error}",
            path.display()
        ));
    }
    cleanup_warning(report, &published.cleanup);
}

fn cleanup_warning(report: &mut InitReport, cleanup: &Cleanup) {
    if let Cleanup::Pending(path) = cleanup {
        report
            .warnings
            .push(format!("cleanup pending: {}", path.display()));
    }
}

#[cfg(all(test, feature = "storage-test-hooks"))]
mod tests {
    use super::*;
    use crate::storage::{LockOptions, TestHooks};
    use std::path::PathBuf;

    #[derive(Debug)]
    struct Directory(PathBuf);

    impl Directory {
        fn new() -> Self {
            let mut random = [0_u8; 16];
            getrandom::fill(&mut random).unwrap();
            let path = std::env::temp_dir().join(format!(
                "cpforge-init-warnings-{:032x}",
                u128::from_le_bytes(random)
            ));
            fs::create_dir(&path).unwrap();
            Self(fs::canonicalize(path).unwrap())
        }
    }

    impl Drop for Directory {
        fn drop(&mut self) {
            // Fixture teardown only.
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    const EXISTING_SEQUENCE: &[u8] =
        b"# concurrent creator\nschema=1\nepoch='0123456789abcdef0123456789abcdef'\nnext='17'\n";

    fn create_valid_sequence(path: &Path) {
        if path.file_name() == Some(std::ffi::OsStr::new("job-seq")) {
            fs::write(path, EXISTING_SEQUENCE).unwrap();
        }
    }

    fn create_malformed_sequence(path: &Path) {
        if path.file_name() == Some(std::ffi::OsStr::new("job-seq")) {
            fs::write(path, b"schema = 1\nnext = '01'\n").unwrap();
        }
    }

    fn create_newer_sequence(path: &Path) {
        if path.file_name() == Some(std::ffi::OsStr::new("job-seq")) {
            fs::write(path, b"schema = 99\n").unwrap();
        }
    }

    #[test]
    fn sequence_publish_collision_is_reclassified_before_config_publication() {
        for (create, classification, bytes) in [
            (
                create_valid_sequence as fn(&Path),
                "valid",
                EXISTING_SEQUENCE,
            ),
            (
                create_malformed_sequence,
                "malformed",
                b"schema = 1\nnext = '01'\n".as_slice(),
            ),
            (
                create_newer_sequence,
                "too_new",
                b"schema = 99\n".as_slice(),
            ),
        ] {
            let dir = Directory::new();
            let lock = WorkspaceLock::new(&dir.0).unwrap();
            let guard = lock
                .acquire_with_options(
                    "init",
                    LockOptions {
                        test_hooks: TestHooks {
                            before_file_publish: Some(create),
                            ..TestHooks::default()
                        },
                        ..LockOptions::default()
                    },
                )
                .unwrap();
            let mut report = InitReport::default();
            let result = initialize_locked(&guard, &mut report);
            if classification == "valid" {
                result.unwrap();
                assert_eq!(
                    fs::read(dir.0.join("cpforge.toml")).unwrap(),
                    b"schema = 1\n"
                );
                assert!(
                    report
                        .lines
                        .iter()
                        .any(|line| line == "kept .cpforge/job-seq")
                );
            } else {
                let error = result.unwrap_err();
                assert_eq!(error.exit_code(), crate::exit::ExitCode::Failure);
                assert!(error.to_string().contains(classification));
                assert!(!dir.0.join("cpforge.toml").exists());
            }
            assert_eq!(fs::read(dir.0.join(".cpforge/job-seq")).unwrap(), bytes);
        }
    }

    #[test]
    fn config_publish_collision_is_success_without_overwriting_the_creator() {
        fn create_config(path: &Path) {
            if path.file_name() == Some(std::ffi::OsStr::new("cpforge.toml")) {
                fs::write(path, b"# concurrent config\nschema = 99\n").unwrap();
            }
        }
        let dir = Directory::new();
        let lock = WorkspaceLock::new(&dir.0).unwrap();
        let guard = lock
            .acquire_with_options(
                "init",
                LockOptions {
                    test_hooks: TestHooks {
                        before_file_publish: Some(create_config),
                        ..TestHooks::default()
                    },
                    ..LockOptions::default()
                },
            )
            .unwrap();
        let mut report = InitReport::default();
        initialize_locked(&guard, &mut report).unwrap();
        assert_eq!(
            fs::read(dir.0.join("cpforge.toml")).unwrap(),
            b"# concurrent config\nschema = 99\n"
        );
        assert!(
            report
                .lines
                .iter()
                .any(|line| line.contains("already initialised"))
        );
        assert!(
            matches!(read_job_sequence(dir.0.join(".cpforge/job-seq")), Record::Valid(sequence) if sequence.next == 0)
        );
    }

    #[test]
    fn recovery_and_durability_warnings_survive_while_init_completes() {
        let dir = Directory::new();
        let leftover = dir.0.join(format!(".cpf-tmp-{}", "a".repeat(32)));
        fs::write(&leftover, b"leftover").unwrap();
        let lock = WorkspaceLock::new(&dir.0).unwrap();
        let guard = lock
            .acquire_with_options(
                "init",
                LockOptions {
                    test_hooks: TestHooks {
                        recovery_remove_failure: Some(leftover.clone()),
                        post_commit_sync_failure: true,
                        ..TestHooks::default()
                    },
                    ..LockOptions::default()
                },
            )
            .unwrap();
        let mut report = InitReport::default();
        initialize_locked(&guard, &mut report).unwrap();
        assert!(
            report
                .warnings
                .iter()
                .any(|warning| warning.contains("leftover recovery incomplete"))
        );
        assert_eq!(
            report
                .warnings
                .iter()
                .filter(|warning| warning.contains("durability uncertain"))
                .count(),
            2
        );
        assert_eq!(
            fs::read(dir.0.join("cpforge.toml")).unwrap(),
            b"schema = 1\n"
        );
        let sequence = fs::read(dir.0.join(".cpforge/job-seq")).unwrap();
        assert!(leftover.exists());
        drop(guard);

        let mut report = InitReport::default();
        initialize(&dir.0, &mut report).unwrap();
        assert!(report.warnings.is_empty());
        assert!(!leftover.exists());
        assert_eq!(fs::read(dir.0.join(".cpforge/job-seq")).unwrap(), sequence);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn pending_publication_cleanup_does_not_retry_or_block_config() {
        let dir = Directory::new();
        let lock = WorkspaceLock::new(&dir.0).unwrap();
        let guard = lock
            .acquire_with_options(
                "init",
                LockOptions {
                    test_hooks: TestHooks {
                        force_link_fallback: true,
                        fail_unlink: true,
                        ..TestHooks::default()
                    },
                    ..LockOptions::default()
                },
            )
            .unwrap();
        let mut report = InitReport::default();
        initialize_locked(&guard, &mut report).unwrap();
        assert_eq!(
            report
                .warnings
                .iter()
                .filter(|warning| warning.contains("cleanup pending"))
                .count(),
            2
        );
        assert_eq!(
            fs::read(dir.0.join("cpforge.toml")).unwrap(),
            b"schema = 1\n"
        );
        let sequence = fs::read(dir.0.join(".cpforge/job-seq")).unwrap();
        for parent in [&dir.0, &dir.0.join(".cpforge")] {
            assert!(fs::read_dir(parent).unwrap().any(|entry| {
                entry
                    .unwrap()
                    .file_name()
                    .to_string_lossy()
                    .starts_with(".cpf-tmp-")
            }));
        }
        drop(guard);

        let mut report = InitReport::default();
        initialize(&dir.0, &mut report).unwrap();
        assert!(report.warnings.is_empty());
        assert_eq!(fs::read(dir.0.join(".cpforge/job-seq")).unwrap(), sequence);
        for parent in [&dir.0, &dir.0.join(".cpforge")] {
            assert!(!fs::read_dir(parent).unwrap().any(|entry| {
                entry
                    .unwrap()
                    .file_name()
                    .to_string_lossy()
                    .starts_with(".cpf-tmp-")
            }));
        }
    }
}
