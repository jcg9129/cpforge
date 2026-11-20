//! Shared-record editing, schema-first reading and lock composition.
mod storage_support;

use std::fs;
use std::path::Path;
use std::process::{Command, Stdio};

use cpforge::storage::{
    EditError, MalformedRecord, Record, WorkspaceLock, edit_toml, publish_dir_noclobber,
    publish_file_noclobber, read_user_record,
};
use storage_support::{Directory, assert_no_temps};
use toml_edit::DocumentMut;

const ORIGINAL: &str = "schema = 1\ncount = 0\n";

fn check(document: &DocumentMut) -> Result<(), &'static str> {
    if document
        .get("schema")
        .and_then(|item| item.as_integer())
        .unwrap_or(1)
        != 1
    {
        return Err("unsupported schema");
    }
    if document
        .get("count")
        .and_then(|item| item.as_integer())
        .is_none()
    {
        return Err("count must be an integer");
    }
    Ok(())
}

fn increment(document: &mut DocumentMut) -> Result<(), &'static str> {
    let count = document["count"]
        .as_integer()
        .ok_or("count must be an integer")?;
    document["count"] = toml_edit::value(count + 1);
    Ok(())
}

fn count(path: &Path) -> i64 {
    match read_user_record(path, 1, |document| {
        document
            .get("count")
            .and_then(|item| item.as_integer())
            .ok_or("count")
    }) {
        Record::Valid(count) => count,
        other => panic!("invalid counter: {other:?}"),
    }
}

#[test]
fn untouched_items_comments_and_formatting_round_trip() {
    let dir = Directory::new();
    let lock = dir.lock();
    let guard = lock.acquire("format preservation").unwrap();
    let original = "# header\nschema = 1 # version\ncount = 0\n\n# unknown table\n[other] # keep\nodd  =  'value'   # keep this too\nlist = [ 1,  2, ]\n";
    fs::write(dir.path("record.toml"), original).unwrap();
    edit_toml(&guard, "record.toml", check, increment).unwrap();
    assert_eq!(
        fs::read_to_string(dir.path("record.toml")).unwrap(),
        original.replace("count = 0", "count = 1")
    );
    // A semantic no-op must reproduce every byte, including the final newline.
    for text in [
        original,
        "schema=1\r\ncount=0\r\n# tail\r\n",
        "schema=1\ncount=0",
        "\u{feff}schema=1\ncount=0",
    ] {
        fs::write(dir.path("record.toml"), text).unwrap();
        edit_toml(&guard, "record.toml", check, |_| Ok(())).unwrap();
        assert_eq!(fs::read(dir.path("record.toml")).unwrap(), text.as_bytes());
    }
    for text in [
        "schema = 1\r\ncount = 0\r\n# tail\r\nother = 'untouched'\r\n",
        "schema = 1\r\ncount = 0\r\nother = \"\"\"\r\nuntouched\nvalue\r\n\"\"\"\n",
        "\u{feff}schema = 1\ncount = 0\nother = 'untouched'",
    ] {
        fs::write(dir.path("record.toml"), text).unwrap();
        edit_toml(&guard, "record.toml", check, increment).unwrap();
        assert_eq!(
            fs::read_to_string(dir.path("record.toml")).unwrap(),
            text.replace("count = 0", "count = 1")
        );
    }
    assert_no_temps(&dir.0);
}

#[test]
fn checks_run_before_operations_and_errors_do_not_publish() {
    let dir = Directory::new();
    let lock = dir.lock();
    let guard = lock.acquire("schema and parse errors").unwrap();
    fs::write(dir.path("record.toml"), "schema = 2\ncount = 0\n").unwrap();
    let error = edit_toml(
        &guard,
        "record.toml",
        check,
        |_| -> Result<(), &'static str> { panic!("operations must not run after a failed check") },
    )
    .unwrap_err();
    assert!(matches!(error, EditError::Check("unsupported schema")));
    for bytes in [b"not = [".as_slice(), b"\xff"] {
        fs::write(dir.path("record.toml"), bytes).unwrap();
        let error = edit_toml(
            &guard,
            "record.toml",
            |_| -> Result<(), ()> { panic!("check must not run on an unparsed record") },
            |_| Ok(()),
        )
        .unwrap_err();
        assert!(matches!(
            error,
            EditError::Parse(_) | EditError::Encoding(_)
        ));
        assert_eq!(fs::read(dir.path("record.toml")).unwrap(), bytes);
    }
    fs::write(dir.path("record.toml"), ORIGINAL).unwrap();
    let error = edit_toml(&guard, "record.toml", check, |document| {
        increment(document)?;
        Err("operation refused")
    })
    .unwrap_err();
    assert!(matches!(error, EditError::Operations("operation refused")));
    assert_eq!(
        fs::read_to_string(dir.path("record.toml")).unwrap(),
        ORIGINAL
    );
    assert_no_temps(&dir.0);
}

#[test]
fn hard_links_and_non_regular_files_are_refused() {
    let dir = Directory::new();
    fs::write(dir.path("record.toml"), ORIGINAL).unwrap();
    fs::hard_link(dir.path("record.toml"), dir.path("alias.toml")).unwrap();
    fs::create_dir(dir.path("directory.toml")).unwrap();
    let lock = dir.lock();
    let guard = lock.acquire("refuse multiply linked records").unwrap();
    for path in ["record.toml", "directory.toml"] {
        assert!(matches!(
            edit_toml(&guard, path, check, increment).unwrap_err(),
            EditError::NotSingleRegularFile { .. }
        ));
    }
    assert_eq!(
        fs::read_to_string(dir.path("alias.toml")).unwrap(),
        ORIGINAL
    );
    assert_no_temps(&dir.0);
}

#[cfg(unix)]
#[test]
fn links_and_fifos_are_refused_without_following_or_blocking() {
    use std::os::unix::{ffi::OsStrExt, fs::symlink};
    let dir = Directory::new();
    fs::write(dir.path("record.toml"), ORIGINAL).unwrap();
    symlink(dir.path("record.toml"), dir.path("link.toml")).unwrap();
    let fifo = std::ffi::CString::new(dir.path("fifo.toml").as_os_str().as_bytes()).unwrap();
    // SAFETY: fifo is terminated and mode is valid.
    assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o600) }, 0);
    let lock = dir.lock();
    let guard = lock.acquire("refuse special records").unwrap();
    assert!(matches!(
        edit_toml(&guard, "link.toml", check, increment).unwrap_err(),
        EditError::Storage(cpforge::storage::StorageError {
            kind: cpforge::storage::ErrorKind::LinkInPath { .. },
            ..
        })
    ));
    assert!(matches!(
        edit_toml(&guard, "fifo.toml", check, increment).unwrap_err(),
        EditError::NotSingleRegularFile { .. }
    ));
    assert!(matches!(
        read_user_record(dir.path("fifo.toml"), 1, check),
        Record::Unreadable(_)
    ));
    assert!(matches!(
        read_user_record(dir.path("link.toml"), 1, check),
        Record::Valid(())
    ));
    assert_no_temps(&dir.0);
}

#[cfg(unix)]
#[test]
fn edits_preserve_unix_mode() {
    use std::os::unix::fs::PermissionsExt;
    let dir = Directory::new();
    fs::write(dir.path("record.toml"), ORIGINAL).unwrap();
    fs::set_permissions(dir.path("record.toml"), fs::Permissions::from_mode(0o640)).unwrap();
    let lock = dir.lock();
    let guard = lock.acquire("mode preservation").unwrap();
    edit_toml(&guard, "record.toml", check, increment).unwrap();
    assert_eq!(
        fs::metadata(dir.path("record.toml"))
            .unwrap()
            .permissions()
            .mode()
            & 0o7777,
        0o640
    );
}

#[test]
fn reader_classifies_missing_unreadable_and_field_errors() {
    let dir = Directory::new();
    assert!(matches!(
        read_user_record(dir.path("absent"), 1, check),
        Record::Missing
    ));
    fs::create_dir(dir.path("directory")).unwrap();
    assert!(matches!(
        read_user_record(dir.path("directory"), 1, check),
        Record::Unreadable(_)
    ));
    fs::write(dir.path("record.toml"), "schema = 1\ncount = []").unwrap();
    assert!(matches!(
        read_user_record(dir.path("record.toml"), 1, check),
        Record::Malformed(MalformedRecord::Fields(_))
    ));
    fs::write(dir.path("record.toml"), "schema = 2\ncount = []").unwrap();
    assert!(matches!(
        read_user_record(dir.path("record.toml"), 1, |_| -> Result<(), ()> {
            panic!("newer records must not be decoded")
        }),
        Record::TooNew { schema: 2, .. }
    ));
}

#[test]
fn nested_steps_borrow_one_guard_and_recovery_precedes_current_artifacts() {
    let dir = Directory::new();
    fs::write(dir.path("record.toml"), ORIGINAL).unwrap();
    let leftover = dir.path(format!(".cpf-tmp-{}", "b".repeat(32)));
    fs::write(&leftover, b"dead transaction").unwrap();
    fs::create_dir_all(dir.path(".cpforge/staging/old/nested")).unwrap();
    let lock = dir.lock();
    let guard = lock.acquire("composed operation").unwrap();
    assert!(!leftover.exists());
    assert!(!dir.path(".cpforge/staging/old").exists());
    assert!(guard.recovery_warning().is_none());
    let stage = guard.create_dir_all(".cpforge/staging/current").unwrap();
    fs::write(stage.join("data"), b"current").unwrap();
    edit_toml(&guard, "record.toml", check, increment).unwrap();
    publish_file_noclobber(&guard, "exclusive", b"current").unwrap();
    // Nested acquisition/recovery would either deadlock or dispose of this stage.
    assert_eq!(fs::read(stage.join("data")).unwrap(), b"current");
    publish_dir_noclobber(&guard, &stage, "published").unwrap();
    assert_eq!(count(&dir.path("record.toml")), 1);
    assert_eq!(fs::read(dir.path("published/data")).unwrap(), b"current");
    assert_no_temps(&dir.0);
}

#[test]
fn two_library_processes_edit_under_one_workspace_lock_without_lost_updates() {
    let dir = Directory::new();
    fs::write(dir.path("record.toml"), ORIGINAL).unwrap();
    let exe = std::env::current_exe().unwrap();
    let mut first = Command::new(&exe)
        .args(["--ignored", "--exact", "edit_child", "--nocapture"])
        .env("CPF_STORAGE_EDIT_ROOT", &dir.0)
        .env_remove("CPF_STORAGE_CRASH_AT")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut second = Command::new(&exe)
        .args(["--ignored", "--exact", "edit_child", "--nocapture"])
        .env("CPF_STORAGE_EDIT_ROOT", &dir.0)
        .env_remove("CPF_STORAGE_CRASH_AT")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    // Both workers announce readiness before either can start their edit loop.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
    while !(dir.path(format!("ready-{}", first.id())).exists()
        && dir.path(format!("ready-{}", second.id())).exists())
    {
        if std::time::Instant::now() >= deadline {
            let _ = first.kill();
            let _ = second.kill();
            let first_output = first.wait_with_output().unwrap();
            let second_output = second.wait_with_output().unwrap();
            panic!("helpers did not become ready: {first_output:?}; {second_output:?}");
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    fs::write(dir.path("go"), b"go").unwrap();
    for child in [first, second] {
        let output = child.wait_with_output().unwrap();
        assert!(output.status.success(), "{output:?}");
    }
    assert_eq!(count(&dir.path("record.toml")), 40);
    assert_no_temps(&dir.0);
}

#[test]
#[ignore]
fn edit_child() {
    let Some(root) = std::env::var_os("CPF_STORAGE_EDIT_ROOT") else {
        return;
    };
    let root = std::path::PathBuf::from(root);
    let lock = WorkspaceLock::new(&root).unwrap();
    fs::write(root.join(format!("ready-{}", std::process::id())), b"ready").unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
    while !root.join("go").exists() {
        assert!(
            std::time::Instant::now() < deadline,
            "parent never released helpers"
        );
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    for _ in 0..20 {
        let guard = lock.acquire("increment shared record").unwrap();
        edit_toml(&guard, "record.toml", check, increment).unwrap();
    }
}

#[cfg(feature = "storage-test-hooks")]
mod races {
    use super::*;
    use cpforge::storage::{Cleanup, LockOptions, TestHooks, edit_toml_replay};

    fn overwrite(path: &Path) {
        fs::write(path, "schema = 1\ncount = 10\n").unwrap();
    }
    fn same_length_edit(path: &Path) {
        fs::write(path, "schema = 1\ncount = 9\n").unwrap();
    }
    fn atomic_save(path: &Path) {
        // Identical bytes with a different identity must still conflict.
        let bytes = fs::read(path).unwrap();
        let saved = path.with_extension("saved");
        fs::write(&saved, bytes).unwrap();
        fs::rename(saved, path).unwrap();
    }
    fn delete(path: &Path) {
        fs::remove_file(path).unwrap();
    }
    fn change_type(path: &Path) {
        delete(path);
        fs::create_dir(path).unwrap();
    }
    fn malformed(path: &Path) {
        fs::write(path, "schema = [").unwrap();
    }
    fn add_link(path: &Path) {
        fs::hard_link(path, path.with_extension("linked")).unwrap();
    }
    fn edit_once(path: &Path) {
        if count(path) == 0 {
            overwrite(path);
        }
    }
    fn edit_always(path: &Path) {
        let next = count(path) + 10;
        fs::write(path, format!("schema = 1\ncount = {next}\n")).unwrap();
    }
    fn newer_schema(path: &Path) {
        fs::write(path, "schema = 2\ncount = 0\n").unwrap();
    }

    fn options(hook: fn(&Path)) -> LockOptions {
        LockOptions {
            test_hooks: TestHooks {
                edit_after_read: Some(hook),
                ..TestHooks::default()
            },
            ..LockOptions::default()
        }
    }

    #[test]
    fn concurrent_edits_saves_deletions_types_malformed_toml_and_links_conflict() {
        for hook in [
            overwrite,
            same_length_edit,
            atomic_save,
            delete,
            change_type,
            malformed,
            add_link,
        ] {
            let dir = Directory::new();
            fs::write(dir.path("record.toml"), ORIGINAL).unwrap();
            let lock = dir.lock();
            let guard = lock
                .acquire_with_options("concurrent edit", options(hook))
                .unwrap();
            let error = edit_toml(&guard, "record.toml", check, increment).unwrap_err();
            assert!(
                matches!(
                    error,
                    EditError::Conflict {
                        cleanup: Cleanup::Done,
                        ..
                    }
                ),
                "{error:?}"
            );
            assert_no_temps(&dir.0);
        }
    }

    #[cfg(unix)]
    #[test]
    fn link_inserted_after_read_is_a_conflict() {
        fn replace_with_link(path: &Path) {
            let saved = path.with_extension("saved");
            fs::rename(path, &saved).unwrap();
            std::os::unix::fs::symlink(saved, path).unwrap();
        }
        let dir = Directory::new();
        fs::write(dir.path("record.toml"), ORIGINAL).unwrap();
        let lock = dir.lock();
        let guard = lock
            .acquire_with_options("linked editor save", options(replace_with_link))
            .unwrap();
        assert!(matches!(
            edit_toml(&guard, "record.toml", check, increment).unwrap_err(),
            EditError::Conflict { .. }
        ));
        assert_eq!(
            fs::read_to_string(dir.path("record.toml")).unwrap(),
            ORIGINAL
        );
        assert_no_temps(&dir.0);
    }

    #[test]
    fn replay_uses_fresh_document_after_one_edit_and_stops_after_two_conflicts() {
        for (hook, succeeds) in [
            (edit_once as fn(&Path), true),
            (edit_always as fn(&Path), false),
        ] {
            let dir = Directory::new();
            fs::write(dir.path("record.toml"), ORIGINAL).unwrap();
            let lock = dir.lock();
            let guard = lock
                .acquire_with_options("explicit replay", options(hook))
                .unwrap();
            let result = edit_toml_replay(&guard, "record.toml", check, increment);
            if succeeds {
                assert!(result.unwrap().committed);
                assert_eq!(count(&dir.path("record.toml")), 11);
            } else {
                assert!(matches!(result.unwrap_err(), EditError::Conflict { .. }));
                assert_eq!(count(&dir.path("record.toml")), 20);
            }
            assert_no_temps(&dir.0);
        }
    }

    #[test]
    fn primitive_never_replays_and_opt_in_rechecks_schema_on_second_read() {
        let dir = Directory::new();
        fs::write(dir.path("record.toml"), ORIGINAL).unwrap();
        let lock = dir.lock();
        let guard = lock
            .acquire_with_options("one attempt", options(edit_always))
            .unwrap();
        assert!(matches!(
            edit_toml(&guard, "record.toml", check, increment).unwrap_err(),
            EditError::Conflict { .. }
        ));
        assert_eq!(count(&dir.path("record.toml")), 10);
        drop(guard);
        fs::write(dir.path("record.toml"), ORIGINAL).unwrap();
        let guard = lock
            .acquire_with_options("schema replay", options(newer_schema))
            .unwrap();
        assert!(matches!(
            edit_toml_replay(&guard, "record.toml", check, increment).unwrap_err(),
            EditError::Check("unsupported schema")
        ));
        assert_eq!(
            fs::read_to_string(dir.path("record.toml")).unwrap(),
            "schema = 2\ncount = 0\n"
        );
        assert_no_temps(&dir.0);
    }

    #[test]
    fn failed_conflict_cleanup_is_retained_across_both_replay_attempts() {
        let dir = Directory::new();
        fs::write(dir.path("record.toml"), ORIGINAL).unwrap();
        let lock = dir.lock();
        let mut settings = options(edit_always);
        settings.test_hooks.fail_unlink = true;
        let guard = lock
            .acquire_with_options("pending conflict cleanup", settings)
            .unwrap();
        let error = edit_toml_replay(&guard, "record.toml", check, increment).unwrap_err();
        let EditError::Conflict {
            cleanup: Cleanup::Pending(temp),
            previous_cleanup: Cleanup::Pending(first_temp),
            ..
        } = error
        else {
            panic!("missing cleanup warning");
        };
        assert!(temp.exists());
        assert!(first_temp.exists());
        assert_ne!(temp, first_temp);
        assert_eq!(count(&dir.path("record.toml")), 20);
        drop(guard);
        let guard = lock.acquire("recover edit temp").unwrap();
        assert!(guard.recovery_warning().is_none());
        assert!(!temp.exists());
        assert!(!first_temp.exists());
        assert_no_temps(&dir.0);
    }

    #[test]
    fn first_attempt_cleanup_warning_survives_success_or_schema_error_on_replay() {
        for (hook, succeeds) in [
            (edit_once as fn(&Path), true),
            (newer_schema as fn(&Path), false),
        ] {
            let dir = Directory::new();
            fs::write(dir.path("record.toml"), ORIGINAL).unwrap();
            let lock = dir.lock();
            let mut settings = options(hook);
            settings.test_hooks.fail_unlink = true;
            let guard = lock
                .acquire_with_options("replay cleanup warning", settings)
                .unwrap();
            let result = edit_toml_replay(&guard, "record.toml", check, increment);
            let cleanup = if succeeds {
                let published = result.unwrap();
                assert!(published.committed);
                assert_eq!(count(&dir.path("record.toml")), 11);
                published.cleanup
            } else {
                let EditError::Replay { error, cleanup } = result.unwrap_err() else {
                    panic!("missing replay cleanup warning");
                };
                assert!(matches!(*error, EditError::Check("unsupported schema")));
                cleanup
            };
            let Cleanup::Pending(temp) = cleanup else {
                panic!("missing temp path");
            };
            assert!(temp.exists());
            drop(guard);
            let guard = lock.acquire("recover first attempt temp").unwrap();
            assert!(guard.recovery_warning().is_none());
            assert!(!temp.exists());
            assert_no_temps(&dir.0);
        }
    }

    #[cfg(windows)]
    #[test]
    fn windows_sharing_retry_revalidates_after_an_editor_save() {
        use std::os::windows::fs::OpenOptionsExt;
        use windows_sys::Win32::Storage::FileSystem::{FILE_SHARE_READ, FILE_SHARE_WRITE};
        fn save_after_first_failure(path: &Path, attempt: usize) {
            assert_eq!(attempt, 1, "second attempt must conflict before publishing");
            overwrite(path);
        }
        let dir = Directory::new();
        fs::write(dir.path("record.toml"), ORIGINAL).unwrap();
        let _blocker = fs::OpenOptions::new()
            .read(true)
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
            .open(dir.path("record.toml"))
            .unwrap();
        let lock = dir.lock();
        let guard = lock
            .acquire_with_options(
                "editor save between retries",
                LockOptions {
                    test_hooks: TestHooks {
                        replace_after_failure: Some(save_after_first_failure),
                        ..TestHooks::default()
                    },
                    ..LockOptions::default()
                },
            )
            .unwrap();
        let error = edit_toml(&guard, "record.toml", check, increment).unwrap_err();
        assert!(matches!(
            error,
            EditError::Conflict {
                cleanup: Cleanup::Done,
                ..
            }
        ));
        assert_eq!(count(&dir.path("record.toml")), 10);
        assert_no_temps(&dir.0);
    }
}

#[cfg(feature = "storage-test-hooks")]
#[test]
fn committed_edit_with_uncertain_durability_continues_the_composed_operation() {
    use cpforge::storage::{Durability, LockOptions, TestHooks};
    let dir = Directory::new();
    fs::write(dir.path("record.toml"), ORIGINAL).unwrap();
    let lock = dir.lock();
    let guard = lock
        .acquire_with_options(
            "uncertain edit",
            LockOptions {
                test_hooks: TestHooks {
                    post_commit_sync_failure: true,
                    ..TestHooks::default()
                },
                ..LockOptions::default()
            },
        )
        .unwrap();
    let result = edit_toml(&guard, "record.toml", check, increment).unwrap();
    assert!(result.committed);
    assert!(matches!(result.durability, Durability::Uncertain(_)));
    assert_eq!(count(&dir.path("record.toml")), 1);
    let next = publish_file_noclobber(&guard, "next-step", b"continued").unwrap();
    assert!(next.committed);
    assert_eq!(fs::read(dir.path("next-step")).unwrap(), b"continued");
    assert_no_temps(&dir.0);
}

#[cfg(not(feature = "storage-test-hooks"))]
#[test]
fn crash_selection_is_inert_without_test_hooks() {
    let dir = Directory::new();
    fs::write(dir.path("record.toml"), ORIGINAL).unwrap();
    let output = Command::new(std::env::current_exe().unwrap())
        .args(["--ignored", "--exact", "single_edit_child", "--nocapture"])
        .env("CPF_STORAGE_SINGLE_EDIT_ROOT", &dir.0)
        .env("CPF_STORAGE_CRASH_AT", "edit_toml.validated")
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    assert!(!String::from_utf8_lossy(&output.stderr).contains("CPF-CRASH-AT"));
    assert_eq!(count(&dir.path("record.toml")), 1);
}

#[test]
#[ignore]
fn single_edit_child() {
    let Some(root) = std::env::var_os("CPF_STORAGE_SINGLE_EDIT_ROOT") else {
        return;
    };
    let lock = WorkspaceLock::new(root).unwrap();
    let guard = lock.acquire("single edit").unwrap();
    edit_toml(&guard, "record.toml", check, increment).unwrap();
}

#[test]
fn unchanged_edit_does_not_replace_the_file() {
    let dir = Directory::new();
    let path = dir.path("record.toml");
    fs::write(&path, ORIGINAL).unwrap();
    let before = fs::metadata(&path).unwrap();
    #[cfg(unix)]
    let identity = {
        use std::os::unix::fs::MetadataExt;
        (before.dev(), before.ino())
    };
    let lock = dir.lock();
    let guard = lock.acquire("unchanged edit").unwrap();
    let published = edit_toml(&guard, &path, check, |_| Ok(())).unwrap();
    assert!(!published.committed);
    let after = fs::metadata(&path).unwrap();
    assert_eq!(before.modified().unwrap(), after.modified().unwrap());
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        assert_eq!(identity, (after.dev(), after.ino()));
    }
    assert_eq!(fs::read(path).unwrap(), ORIGINAL.as_bytes());
    assert_no_temps(&dir.0);
}

#[test]
fn record_size_cap_is_malformed_and_non_directory_ancestors_are_missing() {
    const LIMIT: usize = 1024 * 1024;
    let dir = Directory::new();
    let path = dir.path("record.toml");
    let mut bytes = ORIGINAL.as_bytes().to_vec();
    bytes.push(b'#');
    bytes.resize(LIMIT, b'x');
    fs::write(&path, &bytes).unwrap();
    assert!(matches!(
        read_user_record(&path, 1, check),
        Record::Valid(())
    ));
    bytes.push(b'x');
    fs::write(&path, &bytes).unwrap();
    let record = read_user_record(&path, 1, check);
    assert!(matches!(
        record,
        Record::Malformed(MalformedRecord::TooLarge { limit: LIMIT })
    ));
    if let Record::Malformed(reason) = record {
        assert!(reason.to_string().contains("1048576-byte size limit"));
    }
    assert!(matches!(
        cpforge::storage::classify_user_record(&bytes, 1, check),
        Record::Malformed(MalformedRecord::TooLarge { .. })
    ));
    let lock = dir.lock();
    let guard = lock.acquire("oversize edit").unwrap();
    assert!(matches!(
        edit_toml(&guard, &path, check, |_| Ok(())),
        Err(EditError::TooLarge { .. })
    ));
    assert_eq!(fs::read(&path).unwrap(), bytes);
    assert!(matches!(
        read_user_record(path.join("child.toml"), 1, check),
        Record::Missing
    ));
}

#[test]
fn an_edit_cannot_grow_a_record_past_the_size_cap() {
    let dir = Directory::new();
    fs::write(dir.path("record.toml"), ORIGINAL).unwrap();
    let lock = dir.lock();
    let guard = lock.acquire("oversize output").unwrap();
    let error = edit_toml(&guard, "record.toml", check, |document| {
        document["large"] = toml_edit::value("x".repeat(1024 * 1024));
        Ok(())
    })
    .unwrap_err();
    assert!(matches!(error, EditError::TooLarge { .. }));
    assert_eq!(
        fs::read(dir.path("record.toml")).unwrap(),
        ORIGINAL.as_bytes()
    );
    assert_no_temps(&dir.0);
}

#[cfg(feature = "storage-test-hooks")]
#[test]
fn unchanged_edit_never_attempts_a_temp_write_or_publish() {
    use cpforge::storage::{LockOptions, TestHooks};
    let dir = Directory::new();
    fs::write(dir.path("record.toml"), ORIGINAL).unwrap();
    let lock = dir.lock();
    let guard = lock
        .acquire_with_options(
            "no-op without write",
            LockOptions {
                test_hooks: TestHooks {
                    fail_write_disk_full: true,
                    fail_publish_permission_denied: true,
                    ..TestHooks::default()
                },
                ..LockOptions::default()
            },
        )
        .unwrap();
    let published = edit_toml(&guard, "record.toml", check, |_| Ok(())).unwrap();
    assert!(!published.committed);
    assert_no_temps(&dir.0);
}

#[cfg(feature = "storage-test-hooks")]
#[test]
fn unchanged_edit_still_conflicts_and_can_replay_a_changed_baseline() {
    use cpforge::storage::{LockOptions, TestHooks, edit_toml_replay};
    fn save_once(path: &Path) {
        if count(path) == 0 {
            fs::write(path, "schema = 1\ncount = 1\n").unwrap();
        }
    }
    let dir = Directory::new();
    fs::write(dir.path("record.toml"), ORIGINAL).unwrap();
    let lock = dir.lock();
    let guard = lock
        .acquire_with_options(
            "no-op conflict",
            LockOptions {
                test_hooks: TestHooks {
                    edit_after_read: Some(save_once),
                    ..TestHooks::default()
                },
                ..LockOptions::default()
            },
        )
        .unwrap();
    assert!(matches!(
        edit_toml(&guard, "record.toml", check, |_| Ok(())),
        Err(EditError::Conflict { .. })
    ));
    fs::write(dir.path("record.toml"), ORIGINAL).unwrap();
    assert!(
        !edit_toml_replay(&guard, "record.toml", check, |_| Ok(()))
            .unwrap()
            .committed
    );
    assert_eq!(count(&dir.path("record.toml")), 1);
    assert_no_temps(&dir.0);
}
