//! Storage contract acceptance tests, including real subprocess lock contention.

use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Barrier};
use std::time::{Duration, Instant};

use cpforge::storage::{
    Cleanup, Conflict, ErrorKind, LockOptions, WorkspaceLock, atomic_replace, measure_tree,
    publish_dir_noclobber, publish_file_noclobber, remove_tree,
};

#[derive(Debug)]
struct Directory(PathBuf);

impl Directory {
    fn new() -> Self {
        let mut bits = [0_u8; 16];
        getrandom::fill(&mut bits).unwrap();
        let path = std::env::temp_dir().join(format!(
            "cpforge-storage-{:032x}",
            u128::from_le_bytes(bits)
        ));
        fs::create_dir(&path).unwrap();
        Self(fs::canonicalize(path).unwrap())
    }
    fn lock(&self) -> WorkspaceLock {
        WorkspaceLock::new(&self.0).unwrap()
    }
    fn path(&self, relative: impl AsRef<Path>) -> PathBuf {
        self.0.join(relative)
    }
}

impl Drop for Directory {
    fn drop(&mut self) {
        // Test fixture teardown only; production disposal always uses its own walker.
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn options(timeout: Duration) -> LockOptions {
    LockOptions {
        timeout,
        #[cfg(feature = "storage-test-hooks")]
        test_hooks: cpforge::storage::TestHooks::default(),
    }
}

fn assert_no_temps(directory: &Path) {
    assert!(fs::read_dir(directory).unwrap().all(|entry| {
        !entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with(".cpf-tmp-")
    }));
}

#[test]
fn replace_is_complete_with_an_open_destination_and_uses_exact_temp_name() {
    let dir = Directory::new();
    fs::write(dir.path("record"), b"old").unwrap();
    let mut reader = File::open(dir.path("record")).unwrap();
    let lock = dir.lock();
    let guard = lock.acquire("replace").unwrap();
    let mut calls = 0;
    let published = atomic_replace(&guard, "record", b"complete new record", || {
        calls += 1;
        let names: Vec<_> = fs::read_dir(&dir.0)
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .filter(|name| name.to_string_lossy().starts_with(".cpf-tmp-"))
            .collect();
        assert_eq!(names.len(), 1);
        let name = names[0].to_str().unwrap();
        assert_eq!(name.len(), ".cpf-tmp-".len() + 32);
        assert!(
            name[9..]
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        );
        Ok(())
    })
    .unwrap();
    assert!(published.committed);
    assert_eq!(published.cleanup, Cleanup::Done);
    assert_eq!(calls, 1);
    assert_eq!(
        fs::read(dir.path("record")).unwrap(),
        b"complete new record"
    );
    let mut old = Vec::new();
    reader.read_to_end(&mut old).unwrap();
    assert_eq!(old, b"old");
    assert_no_temps(&dir.0);
}

#[test]
fn validation_failure_keeps_old_file_and_removes_temp() {
    let dir = Directory::new();
    fs::write(dir.path("record"), b"old").unwrap();
    let lock = dir.lock();
    let guard = lock.acquire("replace").unwrap();
    let error = atomic_replace(&guard, "record", b"new", || {
        Err(Conflict {
            path: dir.path("record"),
        })
    })
    .unwrap_err();
    assert!(!error.committed());
    assert!(matches!(error.kind, ErrorKind::Conflict(_)));
    assert_eq!(error.cleanup, Cleanup::Done);
    assert_eq!(fs::read(dir.path("record")).unwrap(), b"old");
    assert_no_temps(&dir.0);
}

#[test]
fn native_replace_failure_preserves_destination() {
    let dir = Directory::new();
    fs::create_dir(dir.path("record")).unwrap();
    fs::write(dir.path("record/child"), b"old").unwrap();
    let lock = dir.lock();
    let guard = lock.acquire("replace").unwrap();
    let error = atomic_replace(&guard, "record", b"new", || Ok(())).unwrap_err();
    assert!(!error.committed());
    assert_eq!(fs::read(dir.path("record/child")).unwrap(), b"old");
    assert_no_temps(&dir.0);
}

#[test]
fn file_publication_is_exclusive_including_existing_empty_directory() {
    let dir = Directory::new();
    let lock = dir.lock();
    let guard = lock.acquire("publish").unwrap();
    assert!(
        publish_file_noclobber(&guard, "record", b"first")
            .unwrap()
            .committed
    );
    let error = publish_file_noclobber(&guard, "record", b"second").unwrap_err();
    assert!(!error.committed());
    assert!(matches!(error.kind, ErrorKind::Exists { .. }));
    assert_eq!(fs::read(dir.path("record")).unwrap(), b"first");
    fs::create_dir(dir.path("empty")).unwrap();
    assert!(matches!(
        publish_file_noclobber(&guard, "empty", b"second")
            .unwrap_err()
            .kind,
        ErrorKind::Exists { .. }
    ));
    assert!(dir.path("empty").is_dir());
    assert_no_temps(&dir.0);
}

#[test]
fn file_and_directory_races_commit_exactly_once() {
    for directory in [false, true] {
        let dir = Directory::new();
        let barrier = Arc::new(Barrier::new(2));
        let threads: Vec<_> = (0..2)
            .map(|index| {
                let root = dir.0.clone();
                let barrier = Arc::clone(&barrier);
                std::thread::spawn(move || {
                    let lock = WorkspaceLock::new(root).unwrap();
                    barrier.wait();
                    let guard = lock.acquire("race").unwrap();
                    let bytes = index.to_string();
                    let result = if directory {
                        let stage = guard
                            .create_dir_all(format!(".cpforge/staging/{index}"))
                            .unwrap();
                        fs::write(stage.join("content"), bytes.as_bytes()).unwrap();
                        publish_dir_noclobber(&guard, stage, "winner")
                    } else {
                        publish_file_noclobber(&guard, "winner", bytes.as_bytes())
                    };
                    match result {
                        Ok(result) => {
                            assert!(result.committed);
                            true
                        }
                        Err(error) => {
                            assert!(matches!(error.kind, ErrorKind::Exists { .. }), "{error:?}");
                            false
                        }
                    }
                })
            })
            .collect();
        assert_eq!(
            threads
                .into_iter()
                .map(|t| usize::from(t.join().unwrap()))
                .sum::<usize>(),
            1
        );
        let content = fs::read(if directory {
            dir.path("winner/content")
        } else {
            dir.path("winner")
        })
        .unwrap();
        assert!(content == b"0" || content == b"1");
    }
}

#[test]
fn directory_publish_syncs_and_moves_complete_tree_and_keeps_existing_target() {
    let dir = Directory::new();
    let lock = dir.lock();
    let guard = lock.acquire("directory").unwrap();
    let stage = guard.create_dir_all(".cpforge/staging/first/sub").unwrap();
    fs::write(stage.join("data"), b"complete").unwrap();
    guard.create_dir_all("problems").unwrap();
    let published =
        publish_dir_noclobber(&guard, ".cpforge/staging/first", "problems/first").unwrap();
    assert!(published.committed);
    assert!(
        matches!(published.durability, cpforge::storage::Durability::Ok),
        "{published:?}"
    );
    assert_eq!(
        fs::read(dir.path("problems/first/sub/data")).unwrap(),
        b"complete"
    );
    assert!(!dir.path(".cpforge/staging/first").exists());
    guard.create_dir_all(".cpforge/staging/second").unwrap();
    guard.create_dir_all("problems/empty").unwrap();
    let error =
        publish_dir_noclobber(&guard, ".cpforge/staging/second", "problems/empty").unwrap_err();
    assert!(matches!(error.kind, ErrorKind::Exists { .. }));
    assert_eq!(
        error.cleanup,
        Cleanup::Pending(dir.path(".cpforge/staging/second"))
    );
    assert!(dir.path(".cpforge/staging/second").is_dir());
    assert!(dir.path("problems/empty").is_dir());
}

#[test]
fn directory_publication_refuses_sources_outside_staging_without_deleting_them() {
    let dir = Directory::new();
    let lock = dir.lock();
    let guard = lock.acquire("source confinement").unwrap();
    let user = guard.create_dir_all("problems/user").unwrap();
    fs::write(user.join("source"), b"keep").unwrap();
    let stage = guard.create_dir_all(".cpforge/staging/tx").unwrap();
    fs::write(stage.join("data"), b"staged").unwrap();
    for source in [
        "problems",
        ".cpforge/staging",
        ".cpforge/staging/../../problems",
    ] {
        for target in ["problems/user/../bad", "problems/user", "new"] {
            let error = publish_dir_noclobber(&guard, source, target).unwrap_err();
            assert!(!error.committed());
            assert!(matches!(error.kind, ErrorKind::InvalidPath { .. }));
            assert_eq!(fs::read(user.join("source")).unwrap(), b"keep");
            assert_eq!(fs::read(stage.join("data")).unwrap(), b"staged");
            assert!(!dir.path("new").exists());
        }
    }
}

#[test]
fn invalid_directory_target_retains_the_staged_source() {
    let dir = Directory::new();
    let lock = dir.lock();
    let guard = lock.acquire("target validation").unwrap();
    let user = guard.create_dir_all("problems/user").unwrap();
    fs::write(user.join("source"), b"keep").unwrap();
    let other = guard.create_dir_all(".cpforge/staging/other").unwrap();
    fs::write(other.join("data"), b"other transaction").unwrap();
    for target in ["problems/user/../bad", "problems/bad:name"] {
        let stage = guard.create_dir_all(".cpforge/staging/tx").unwrap();
        fs::write(stage.join("data"), b"staged").unwrap();
        let error = publish_dir_noclobber(&guard, &stage, target).unwrap_err();
        assert!(!error.committed());
        assert_eq!(error.cleanup, Cleanup::Pending(stage.clone()));
        assert_eq!(fs::read(stage.join("data")).unwrap(), b"staged");
        assert_eq!(fs::read(user.join("source")).unwrap(), b"keep");
        assert_eq!(fs::read(other.join("data")).unwrap(), b"other transaction");
    }
}

#[test]
fn overlapping_directory_paths_retain_staging_and_report_pending() {
    let dir = Directory::new();
    let lock = dir.lock();
    let guard = lock.acquire("overlap").unwrap();
    let stage = guard.create_dir_all(".cpforge/staging/tx").unwrap();
    fs::write(stage.join("data"), b"staged").unwrap();
    for target in [
        ".cpforge/staging",
        ".cpforge/staging/tx",
        ".cpforge/staging/tx/child",
    ] {
        let error = publish_dir_noclobber(&guard, &stage, target).unwrap_err();
        assert!(!error.committed());
        assert!(matches!(error.kind, ErrorKind::InvalidPath { .. }));
        assert_eq!(error.cleanup, Cleanup::Pending(stage.clone()));
        assert_eq!(fs::read(stage.join("data")).unwrap(), b"staged");
    }
}

#[test]
fn source_path_refusal_retains_staging_and_reports_pending() {
    let dir = Directory::new();
    let lock = dir.lock();
    let guard = lock.acquire("source link").unwrap();
    let stage = guard.create_dir_all(".cpforge/staging/tx").unwrap();
    let user = guard.create_dir_all("problems/user").unwrap();
    fs::write(user.join("source"), b"keep").unwrap();
    let link = stage.join("link");
    symlink_directory(&user, &link);
    let error = publish_dir_noclobber(&guard, &link, "published").unwrap_err();
    assert!(!error.committed());
    assert!(matches!(error.kind, ErrorKind::LinkInPath { .. }));
    assert_eq!(error.cleanup, Cleanup::Pending(link));
    assert!(stage.exists());
    assert_eq!(fs::read(user.join("source")).unwrap(), b"keep");
}

#[cfg(unix)]
#[test]
fn unwritable_publication_directory_keeps_old_content_and_cleans_temp() {
    use std::os::unix::fs::PermissionsExt;
    // Permission bits cannot deny directory writes to root.
    // SAFETY: geteuid has no preconditions or pointer arguments.
    if unsafe { libc::geteuid() } == 0 {
        return;
    }
    let dir = Directory::new();
    let lock = dir.lock();
    let guard = lock.acquire("publication permissions").unwrap();
    let parent = guard.create_dir_all("records").unwrap();
    let target = parent.join("record");
    fs::write(&target, b"old").unwrap();
    let original = fs::metadata(&parent).unwrap().permissions();
    // Deny temp creation for both file publication primitives.
    fs::set_permissions(&parent, fs::Permissions::from_mode(0o555)).unwrap();
    let replace = atomic_replace(&guard, &target, b"new", || Ok(()));
    let exclusive = publish_file_noclobber(&guard, parent.join("absent"), b"new");
    fs::set_permissions(&parent, original).unwrap();
    for result in [replace, exclusive] {
        let error = result.unwrap_err();
        assert!(!error.committed());
        assert!(matches!(error.kind, ErrorKind::Io { source, .. }
            if source.kind() == std::io::ErrorKind::PermissionDenied));
        assert_eq!(error.cleanup, Cleanup::Done);
    }
    assert_eq!(fs::read(&target).unwrap(), b"old");
    assert!(!parent.join("absent").exists());
    assert_no_temps(&parent);
}

#[test]
fn maximum_component_length_works_for_both_publications() {
    let dir = Directory::new();
    let lock = dir.lock();
    let guard = lock.acquire("long names").unwrap();
    let replace = "a".repeat(255);
    let exclusive = "b".repeat(255);
    atomic_replace(&guard, &replace, b"first", || Ok(())).unwrap();
    atomic_replace(&guard, &replace, b"replacement", || Ok(())).unwrap();
    publish_file_noclobber(&guard, &exclusive, b"exclusive").unwrap();
    assert_eq!(fs::read(dir.path(replace)).unwrap(), b"replacement");
    assert_eq!(fs::read(dir.path(exclusive)).unwrap(), b"exclusive");
}

#[test]
fn native_name_too_long_is_mapped() {
    let dir = Directory::new();
    let lock = dir.lock();
    let guard = lock.acquire("length error").unwrap();
    assert!(matches!(
        guard.check_path("x".repeat(300)).unwrap_err().kind,
        ErrorKind::PathTooLong { .. }
    ));
}

#[test]
fn recovery_matches_exact_regular_files_in_all_metadata_locations() {
    let dir = Directory::new();
    let locations = [
        "",
        ".cpforge",
        "problems/a",
        "problems/a/tests",
        "contests/a",
        ".cpforge/facts/a",
    ];
    let exact = format!(".cpf-tmp-{}", "0123456789abcdef".repeat(2));
    let short = format!(".cpf-tmp-{}", "a".repeat(31));
    let uppercase = format!(".cpf-tmp-{}", "A".repeat(32));
    let exact_directory = format!(".cpf-tmp-{}", "b".repeat(32));
    for location in locations {
        let location = dir.path(location);
        fs::create_dir_all(&location).unwrap();
        for name in [&exact, &short, &uppercase] {
            fs::write(location.join(name), b"leftover").unwrap();
        }
        fs::create_dir(location.join(&exact_directory)).unwrap();
    }
    fs::create_dir_all(dir.path(".cpforge/staging/anything/nested")).unwrap();
    fs::write(
        dir.path(".cpforge/staging/anything/nested/file"),
        b"leftover",
    )
    .unwrap();
    fs::write(dir.path(".cpforge/staging/ordinary"), b"leftover").unwrap();
    fs::create_dir_all(dir.path(".cpforge/runs/job")).unwrap();
    fs::write(dir.path(".cpforge/runs/job").join(&exact), b"job-owned").unwrap();
    let lock = dir.lock();
    let guard = lock.acquire("recover").unwrap();
    assert!(
        guard.recovery_warning().is_none(),
        "{:?}",
        guard.recovery_warning()
    );
    for location in locations {
        let location = dir.path(location);
        assert!(!location.join(&exact).exists());
        assert!(location.join(&short).is_file());
        assert!(location.join(&uppercase).is_file());
        assert!(location.join(&exact_directory).is_dir());
    }
    assert!(dir.path(".cpforge/runs/job").join(exact).is_file());
    assert_eq!(
        fs::read_dir(dir.path(".cpforge/staging")).unwrap().count(),
        0
    );
    // Recovery runs only at outer entry, never when internal steps borrow guard.
    let current = dir.path(format!(".cpf-tmp-{}", "c".repeat(32)));
    fs::write(&current, b"current transaction").unwrap();
    publish_file_noclobber(&guard, "record", b"okay").unwrap();
    assert!(current.exists());
}

#[test]
fn local_lock_is_bounded_and_non_reentrant_across_separate_handles() {
    let dir = Directory::new();
    let first = dir.lock();
    let second = dir.lock();
    let held = first.acquire("first").unwrap();
    for lock in [&first, &second] {
        let start = Instant::now();
        let error = lock
            .acquire_with_options("second", options(Duration::from_millis(30)))
            .unwrap_err();
        assert!(matches!(error.kind, ErrorKind::Busy { ref op } if op == "second"));
        assert!(start.elapsed() >= Duration::from_millis(30));
        assert!(start.elapsed() < Duration::from_secs(2));
    }
    drop(held);
    assert!(
        second
            .acquire_with_options("after drop", options(Duration::ZERO))
            .is_ok()
    );
}

#[test]
fn separate_process_observes_os_lock_and_acquires_after_release() {
    let dir = Directory::new();
    let lock = dir.lock();
    let held = lock.acquire("parent").unwrap();
    publish_file_noclobber(&held, "parent-record", b"parent").unwrap();
    let run = |busy: bool| {
        let output = Command::new(std::env::current_exe().unwrap())
            .args(["--ignored", "--exact", "lock_child", "--nocapture"])
            .env("CPF_STORAGE_LOCK_ROOT", &dir.0)
            .env("CPF_STORAGE_LOCK_BUSY", if busy { "yes" } else { "no" })
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    };
    run(true);
    assert!(!dir.path("child-record").exists());
    drop(held);
    run(false);
    assert_eq!(fs::read(dir.path("child-record")).unwrap(), b"child");
}

#[test]
#[ignore = "subprocess helper"]
fn lock_child() {
    let root = std::env::var_os("CPF_STORAGE_LOCK_ROOT").expect("helper root");
    let lock = WorkspaceLock::new(root).unwrap();
    let result = lock.acquire_with_options("child", options(Duration::from_millis(60)));
    if std::env::var("CPF_STORAGE_LOCK_BUSY").unwrap() == "yes" {
        assert!(matches!(result.unwrap_err().kind, ErrorKind::Busy { .. }));
    } else {
        let guard = result.unwrap();
        publish_file_noclobber(&guard, "child-record", b"child").unwrap();
    }
}

#[test]
fn lock_handle_is_not_inherited_by_spawned_child() {
    let dir = Directory::new();
    let lock = dir.lock();
    let guard = lock.acquire("parent").unwrap();
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args(["--ignored", "--exact", "handle_child", "--nocapture"])
        .env("CPF_STORAGE_LOCK_ROOT", &dir.0)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    // The helper waits while inspecting inherited handles, while the guard is
    // still held. Unlocking in the parent cannot mask descriptor inheritance.
    let mut output = String::new();
    use std::io::BufRead;
    let mut reader = std::io::BufReader::new(child.stdout.take().unwrap());
    loop {
        output.clear();
        assert_ne!(reader.read_line(&mut output).unwrap(), 0);
        if output.contains("HANDLES-CHECKED") {
            break;
        }
    }
    child.stdin.take().unwrap().write_all(b"go").unwrap();
    assert!(child.wait().unwrap().success());
    drop(guard);
}

#[test]
#[ignore = "subprocess helper"]
#[allow(clippy::unnecessary_cast)] // Native stat widths differ on Linux and Darwin.
fn handle_child() {
    let root = PathBuf::from(std::env::var_os("CPF_STORAGE_LOCK_ROOT").expect("helper root"));
    #[cfg(target_os = "linux")]
    {
        let lock = root.join(".cpforge/lock");
        for entry in fs::read_dir("/proc/self/fd").unwrap() {
            if let Ok(target) = fs::read_link(entry.unwrap().path()) {
                assert_ne!(target, lock);
            }
        }
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let metadata = fs::metadata(root.join(".cpforge/lock")).unwrap();
        // The subprocess starts with a small descriptor table. Verify identities
        // as well as CLOEXEC behavior on Darwin, where /proc is unavailable.
        for fd in 3..4096 {
            let mut stat = std::mem::MaybeUninit::<libc::stat>::uninit();
            // SAFETY: stat is writable; fstat only inspects the numbered descriptor.
            if unsafe { libc::fstat(fd, stat.as_mut_ptr()) } == 0 {
                // SAFETY: successful fstat initialized stat.
                let stat = unsafe { stat.assume_init() };
                assert!(
                    stat.st_dev as u64 != metadata.dev() || stat.st_ino as u64 != metadata.ino(),
                    "inherited lock fd {fd}"
                );
            }
        }
    }
    #[cfg(windows)]
    {
        use std::os::windows::io::AsRawHandle;
        use windows_sys::Win32::{
            Foundation::HANDLE,
            Storage::FileSystem::{BY_HANDLE_FILE_INFORMATION, GetFileInformationByHandle},
        };
        let file = File::open(root.join(".cpforge/lock")).unwrap();
        let mut expected = BY_HANDLE_FILE_INFORMATION::default();
        // SAFETY: file is live and expected is writable.
        assert_ne!(
            unsafe { GetFileInformationByHandle(file.as_raw_handle(), &mut expected) },
            0
        );
        // Inherited handle values retain their numeric values in a child. File
        // identities avoid mistaking unrelated runtime handles for this lock.
        for value in (4..65536).step_by(4) {
            let handle = value as HANDLE;
            if handle == file.as_raw_handle() {
                continue;
            }
            let mut info = BY_HANDLE_FILE_INFORMATION::default();
            // SAFETY: this query does not take ownership; invalid handle values
            // return an error, and info is valid writable storage.
            if unsafe { GetFileInformationByHandle(handle, &mut info) } != 0 {
                assert!(
                    (
                        info.dwVolumeSerialNumber,
                        info.nFileIndexHigh,
                        info.nFileIndexLow
                    ) != (
                        expected.dwVolumeSerialNumber,
                        expected.nFileIndexHigh,
                        expected.nFileIndexLow
                    ),
                    "inherited lock handle {value}"
                );
            }
        }
    }
    println!("HANDLES-CHECKED");
    std::io::stdout().flush().unwrap();
    let mut input = [0_u8; 2];
    std::io::stdin().read_exact(&mut input).unwrap();
}

#[cfg(unix)]
#[test]
fn unix_mode_is_preserved() {
    use std::os::unix::fs::PermissionsExt;
    let dir = Directory::new();
    let lock = dir.lock();
    let guard = lock.acquire("mode").unwrap();
    for mode in [0o600, 0o751] {
        fs::write(dir.path("record"), b"old").unwrap();
        fs::set_permissions(dir.path("record"), fs::Permissions::from_mode(mode)).unwrap();
        atomic_replace(&guard, "record", b"new", || Ok(())).unwrap();
        assert_eq!(
            fs::metadata(dir.path("record"))
                .unwrap()
                .permissions()
                .mode()
                & 0o7777,
            mode
        );
    }
}

fn symlink_directory(source: &Path, target: &Path) {
    #[cfg(unix)]
    std::os::unix::fs::symlink(source, target).unwrap();
    #[cfg(windows)]
    {
        // Junctions exercise reparse handling without requiring developer mode.
        let ordinary = |path: &Path| {
            let path = path.as_os_str().to_string_lossy();
            if let Some(unc) = path.strip_prefix(r"\\?\UNC\") {
                PathBuf::from(format!(r"\\{unc}"))
            } else {
                PathBuf::from(path.strip_prefix(r"\\?\").unwrap_or(&path))
            }
        };
        let status = Command::new("cmd.exe")
            .args(["/C", "mklink", "/J"])
            .arg(ordinary(target))
            .arg(ordinary(source))
            .status()
            .unwrap();
        assert!(status.success());
        assert_eq!(
            fs::canonicalize(target).unwrap(),
            fs::canonicalize(source).unwrap()
        );
    }
}

#[test]
fn walker_removes_directory_links_without_touching_outside_data() {
    let dir = Directory::new();
    let outside = Directory::new();
    fs::write(outside.path("valuable"), b"keep").unwrap();
    let lock = dir.lock();
    let guard = lock.acquire("walker").unwrap();
    guard.create_dir_all("tree/nested").unwrap();
    fs::write(dir.path("tree/nested/data"), b"12345").unwrap();
    symlink_directory(&outside.0, &dir.path("tree/outside"));
    let measured = measure_tree(&guard, "tree").unwrap();
    assert_eq!(measured.bytes, 5);
    assert!(measured.errors.is_empty());
    assert!(measured.skipped.is_empty());
    let deleted = remove_tree(&guard, "tree").unwrap();
    assert_eq!(deleted.bytes, 5);
    assert!(deleted.errors.is_empty(), "{deleted:?}");
    assert!(!dir.path("tree").exists());
    assert_eq!(fs::read(outside.path("valuable")).unwrap(), b"keep");
    symlink_directory(&outside.0, &dir.path("link"));
    assert!(matches!(
        guard.check_path("link/valuable").unwrap_err().kind,
        ErrorKind::LinkInPath { .. }
    ));
    assert!(matches!(
        atomic_replace(&guard, "link/valuable", b"bad", || Ok(()))
            .unwrap_err()
            .kind,
        ErrorKind::LinkInPath { .. }
    ));
    let deleted = remove_tree(&guard, "link").unwrap();
    assert!(deleted.errors.is_empty());
    assert_eq!(fs::read(outside.path("valuable")).unwrap(), b"keep");
}

#[cfg(unix)]
#[test]
fn walker_removes_file_links_and_special_entries_without_opening_them() {
    use std::os::unix::{ffi::OsStrExt, fs::symlink};
    let dir = Directory::new();
    let outside = Directory::new();
    fs::write(outside.path("data"), b"valuable").unwrap();
    let lock = dir.lock();
    let guard = lock.acquire("special files").unwrap();
    guard.create_dir_all("tree").unwrap();
    symlink(outside.path("data"), dir.path("tree/link")).unwrap();
    symlink("missing", dir.path("tree/dangling")).unwrap();
    let fifo = std::ffi::CString::new(dir.path("tree/fifo").as_os_str().as_bytes()).unwrap();
    // SAFETY: fifo is a valid terminated path and no pointer is retained.
    assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o600) }, 0);
    assert_eq!(measure_tree(&guard, "tree").unwrap().bytes, 0);
    assert!(remove_tree(&guard, "tree").unwrap().errors.is_empty());
    assert_eq!(fs::read(outside.path("data")).unwrap(), b"valuable");
}

#[test]
fn root_reached_through_link_is_canonicalized_once() {
    let dir = Directory::new();
    let alias = Directory::new();
    symlink_directory(&dir.0, &alias.path("workspace"));
    let lock = WorkspaceLock::new(alias.path("workspace")).unwrap();
    let guard = lock.acquire("root alias").unwrap();
    assert_eq!(guard.root(), dir.0.as_path());
    publish_file_noclobber(&guard, "record", b"okay").unwrap();
    assert_eq!(fs::read(dir.path("record")).unwrap(), b"okay");
}

#[cfg(windows)]
#[test]
fn windows_sharing_violation_retries_and_preserves_old_content() {
    use std::os::windows::fs::OpenOptionsExt;
    use windows_sys::Win32::Storage::FileSystem::{FILE_SHARE_READ, FILE_SHARE_WRITE};
    let dir = Directory::new();
    fs::write(dir.path("record"), b"old").unwrap();
    let _held = fs::OpenOptions::new()
        .read(true)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
        .open(dir.path("record"))
        .unwrap();
    let lock = dir.lock();
    let guard = lock.acquire("sharing violation").unwrap();
    let mut attempts = 0;
    let start = Instant::now();
    let error = atomic_replace(&guard, "record", b"new", || {
        attempts += 1;
        Ok(())
    })
    .unwrap_err();
    assert!(!error.committed());
    assert!(attempts > 1);
    assert!(start.elapsed() >= Duration::from_millis(1900));
    assert!(start.elapsed() < Duration::from_secs(4));
    assert_eq!(fs::read(dir.path("record")).unwrap(), b"old");
    assert_no_temps(&dir.0);
}

#[cfg(windows)]
#[test]
fn windows_retry_revalidates_and_detects_conflict() {
    use std::os::windows::fs::OpenOptionsExt;
    use windows_sys::Win32::Storage::FileSystem::{FILE_SHARE_READ, FILE_SHARE_WRITE};
    let dir = Directory::new();
    fs::write(dir.path("record"), b"old").unwrap();
    let _held = fs::OpenOptions::new()
        .read(true)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
        .open(dir.path("record"))
        .unwrap();
    let lock = dir.lock();
    let guard = lock.acquire("retry conflict").unwrap();
    let mut attempts = 0;
    let error = atomic_replace(&guard, "record", b"new", || {
        attempts += 1;
        if attempts > 1 {
            return Err(Conflict {
                path: dir.path("record"),
            });
        }
        fs::write(dir.path("record"), b"editor").unwrap();
        Ok(())
    })
    .unwrap_err();
    assert!(matches!(error.kind, ErrorKind::Conflict(_)));
    assert_eq!(attempts, 2);
    assert_eq!(fs::read(dir.path("record")).unwrap(), b"editor");
    assert_no_temps(&dir.0);
}

#[cfg(windows)]
#[test]
fn windows_verbatim_paths_exceed_260_utf16_units() {
    use std::os::windows::ffi::OsStrExt;
    let dir = Directory::new();
    let root = dir.path(
        PathBuf::from("long-workspace-segment".repeat(4))
            .join("another-long-segment".repeat(4))
            .join("third-long-segment".repeat(4)),
    );
    fs::create_dir_all(&root).unwrap();
    assert!(root.as_os_str().encode_wide().count() > 260);
    let lock = WorkspaceLock::new(root.clone()).unwrap();
    let guard = lock.acquire("long root").unwrap();
    atomic_replace(&guard, "record", b"first", || Ok(())).unwrap();
    publish_file_noclobber(&guard, "exclusive", b"second").unwrap();
    guard.create_dir_all(".cpforge/staging/source/sub").unwrap();
    fs::write(root.join(".cpforge/staging/source/sub/data"), b"third").unwrap();
    publish_dir_noclobber(&guard, ".cpforge/staging/source", "directory").unwrap();
    assert_eq!(fs::read(root.join("record")).unwrap(), b"first");
    assert_eq!(fs::read(root.join("exclusive")).unwrap(), b"second");
    assert_eq!(measure_tree(&guard, "directory").unwrap().bytes, 5);
    assert!(remove_tree(&guard, "directory").unwrap().errors.is_empty());
}

#[cfg(unix)]
#[test]
fn native_recovery_permission_failures_are_warnings_and_operation_continues() {
    use std::os::unix::fs::PermissionsExt;
    // Permission bits cannot make root unable to enumerate or unlink.
    // The deterministic hooks below exercise this case on every CI account.
    // SAFETY: geteuid has no preconditions or pointer arguments.
    if unsafe { libc::geteuid() } == 0 {
        return;
    }
    let dir = Directory::new();
    fs::create_dir_all(dir.path(".cpforge")).unwrap();
    fs::create_dir_all(dir.path("problems/unreadable")).unwrap();
    let leftover = dir.path(format!(".cpf-tmp-{}", "a".repeat(32)));
    fs::write(&leftover, b"undeletable").unwrap();
    fs::set_permissions(&dir.0, fs::Permissions::from_mode(0o555)).unwrap();
    fs::set_permissions(
        dir.path("problems/unreadable"),
        fs::Permissions::from_mode(0o000),
    )
    .unwrap();
    let lock = dir.lock();
    let guard = lock.acquire("recover permissions").unwrap();
    let result = publish_file_noclobber(&guard, ".cpforge/record", b"continues");
    fs::set_permissions(&dir.0, fs::Permissions::from_mode(0o755)).unwrap();
    fs::set_permissions(
        dir.path("problems/unreadable"),
        fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    let warning = guard.recovery_warning().unwrap();
    assert!(warning.paths.contains(&leftover));
    assert!(warning.paths.contains(&dir.path("problems/unreadable")));
    assert!(leftover.exists());
    assert!(result.unwrap().committed);
}

#[cfg(windows)]
#[test]
fn windows_open_leftover_is_reported_without_blocking_operation() {
    use std::os::windows::fs::OpenOptionsExt;
    use windows_sys::Win32::Storage::FileSystem::{FILE_SHARE_READ, FILE_SHARE_WRITE};

    let dir = Directory::new();
    let leftover = dir.path(format!(".cpf-tmp-{}", "a".repeat(32)));
    fs::write(&leftover, b"held open").unwrap();
    // Omit FILE_SHARE_DELETE so both deletion and a DELETE-access reopen fail.
    let held = fs::OpenOptions::new()
        .read(true)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
        .open(&leftover)
        .unwrap();
    let lock = dir.lock();
    let guard = lock.acquire("recover open leftover").unwrap();
    assert!(guard.recovery_warning().unwrap().paths.contains(&leftover));
    assert!(leftover.exists());
    assert!(
        publish_file_noclobber(&guard, "record", b"continues")
            .unwrap()
            .committed
    );
    drop(held);
}

#[cfg(feature = "storage-test-hooks")]
mod failures {
    use super::*;
    use cpforge::storage::{Durability, TestHooks};

    fn hooks(test_hooks: TestHooks) -> LockOptions {
        LockOptions {
            test_hooks,
            ..LockOptions::default()
        }
    }

    #[test]
    fn post_commit_sync_failure_returns_success_for_all_publications() {
        let dir = Directory::new();
        let lock = dir.lock();
        let guard = lock
            .acquire_with_options(
                "uncertain",
                hooks(TestHooks {
                    post_commit_sync_failure: true,
                    ..TestHooks::default()
                }),
            )
            .unwrap();
        let replace = atomic_replace(&guard, "replace", b"new", || Ok(())).unwrap();
        let exclusive = publish_file_noclobber(&guard, "exclusive", b"new").unwrap();
        guard.create_dir_all(".cpforge/staging/tx").unwrap();
        fs::write(dir.path(".cpforge/staging/tx/data"), b"new").unwrap();
        let directory = publish_dir_noclobber(&guard, ".cpforge/staging/tx", "directory").unwrap();
        for result in [replace, exclusive, directory] {
            assert!(result.committed);
            assert!(matches!(result.durability, Durability::Uncertain(_)));
            assert_eq!(result.cleanup, Cleanup::Done);
        }
        for path in ["replace", "exclusive", "directory/data"] {
            assert_eq!(fs::read(dir.path(path)).unwrap(), b"new");
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn link_fallback_commit_with_failed_unlink_is_pending_and_recoverable() {
        let dir = Directory::new();
        let lock = dir.lock();
        let guard = lock
            .acquire_with_options(
                "link fallback",
                hooks(TestHooks {
                    force_link_fallback: true,
                    fail_unlink: true,
                    ..TestHooks::default()
                }),
            )
            .unwrap();
        let result = publish_file_noclobber(&guard, "record", b"committed").unwrap();
        assert!(result.committed);
        let Cleanup::Pending(temp) = result.cleanup else {
            panic!("expected pending unlink");
        };
        assert_eq!(fs::read(&temp).unwrap(), b"committed");
        assert_eq!(fs::read(dir.path("record")).unwrap(), b"committed");
        let error = publish_file_noclobber(&guard, "record", b"racing").unwrap_err();
        assert!(!error.committed());
        assert!(matches!(error.kind, ErrorKind::Exists { .. }));
        assert!(matches!(error.cleanup, Cleanup::Pending(_)));
        drop(guard);
        let guard = lock.acquire("recover link").unwrap();
        assert!(guard.recovery_warning().is_none());
        assert!(!temp.exists());
        assert_eq!(fs::read(dir.path("record")).unwrap(), b"committed");
    }

    #[test]
    fn pre_commit_sync_failure_keeps_target_and_reports_failed_temp_cleanup() {
        let dir = Directory::new();
        fs::write(dir.path("record"), b"old").unwrap();
        let lock = dir.lock();
        let guard = lock
            .acquire_with_options(
                "pre-commit failure",
                hooks(TestHooks {
                    fail_pre_commit_sync: true,
                    fail_unlink: true,
                    ..TestHooks::default()
                }),
            )
            .unwrap();
        let error = atomic_replace(&guard, "record", b"new", || Ok(())).unwrap_err();
        assert!(!error.committed());
        let Cleanup::Pending(temp) = error.cleanup else {
            panic!("expected pending cleanup");
        };
        assert!(temp.exists());
        assert_eq!(fs::read(dir.path("record")).unwrap(), b"old");
        let error = publish_file_noclobber(&guard, "absent", b"new").unwrap_err();
        assert!(!error.committed());
        assert!(!dir.path("absent").exists());
        guard.create_dir_all(".cpforge/staging/tx").unwrap();
        fs::write(dir.path(".cpforge/staging/tx/data"), b"new").unwrap();
        let error = publish_dir_noclobber(&guard, ".cpforge/staging/tx", "directory").unwrap_err();
        assert!(!error.committed());
        assert!(!dir.path("directory").exists());
        assert_eq!(
            error.cleanup,
            Cleanup::Pending(dir.path(".cpforge/staging/tx"))
        );
        assert_eq!(
            fs::read(dir.path(".cpforge/staging/tx/data")).unwrap(),
            b"new"
        );
    }

    #[test]
    fn exclusive_directory_unsupported_and_cross_device_retain_staging() {
        for cross_device in [false, true] {
            let dir = Directory::new();
            let lock = dir.lock();
            let guard = lock
                .acquire_with_options(
                    "directory refusal",
                    hooks(TestHooks {
                        force_directory_unsupported: !cross_device,
                        force_cross_filesystem: cross_device,
                        ..TestHooks::default()
                    }),
                )
                .unwrap();
            guard.create_dir_all(".cpforge/staging/tx").unwrap();
            fs::write(dir.path(".cpforge/staging/tx/data"), b"new").unwrap();
            let error =
                publish_dir_noclobber(&guard, ".cpforge/staging/tx", "directory").unwrap_err();
            assert!(!error.committed());
            assert_eq!(
                error.cleanup,
                Cleanup::Pending(dir.path(".cpforge/staging/tx"))
            );
            if cross_device {
                assert!(matches!(error.kind, ErrorKind::CrossFilesystem { .. }));
            } else {
                assert!(matches!(error.kind, ErrorKind::Unsupported { .. }));
            }
            assert!(!dir.path("directory").exists());
            assert_eq!(
                fs::read(dir.path(".cpforge/staging/tx/data")).unwrap(),
                b"new"
            );
            drop(guard);
            let recovered = lock.acquire("recover retained staging").unwrap();
            assert!(recovered.recovery_warning().is_none());
            assert!(!dir.path(".cpforge/staging/tx").exists());
        }
    }

    #[test]
    fn recovery_failures_are_warnings_and_do_not_block_publication() {
        let dir = Directory::new();
        let leftover = dir.path(format!(".cpf-tmp-{}", "a".repeat(32)));
        fs::write(&leftover, b"leftover").unwrap();
        fs::create_dir_all(dir.path("problems/unreadable")).unwrap();
        let lock = dir.lock();
        let guard = lock
            .acquire_with_options(
                "recover failure",
                hooks(TestHooks {
                    recovery_remove_failure: Some(leftover.clone()),
                    recovery_read_failure: Some(dir.path("problems/unreadable")),
                    ..TestHooks::default()
                }),
            )
            .unwrap();
        let warning = guard.recovery_warning().unwrap();
        assert!(warning.paths.contains(&leftover));
        assert!(warning.paths.contains(&dir.path("problems/unreadable")));
        assert_eq!(warning.paths.len(), warning.errors.len());
        assert!(leftover.exists());
        assert!(
            publish_file_noclobber(&guard, "record", b"continues")
                .unwrap()
                .committed
        );
    }
}

#[cfg(feature = "storage-test-hooks")]
#[test]
fn walker_skips_boundaries_and_preserves_all_containing_directories() {
    use cpforge::storage::{SkipReason, TestHooks};
    for mount in [false, true] {
        let dir = Directory::new();
        fs::create_dir_all(dir.path("tree/nested/boundary")).unwrap();
        fs::write(dir.path("tree/nested/boundary/keep"), b"outside").unwrap();
        fs::write(dir.path("tree/ordinary"), b"123").unwrap();
        let boundary = dir.path("tree/nested/boundary");
        let test_hooks = if mount {
            TestHooks {
                walker_mount_point: Some(boundary.clone()),
                ..TestHooks::default()
            }
        } else {
            TestHooks {
                walker_device_boundary: Some(boundary.clone()),
                ..TestHooks::default()
            }
        };
        let lock = dir.lock();
        let guard = lock
            .acquire_with_options(
                "boundary",
                LockOptions {
                    test_hooks,
                    ..LockOptions::default()
                },
            )
            .unwrap();
        let measured = measure_tree(&guard, "tree").unwrap();
        assert_eq!(measured.bytes, 3);
        assert_eq!(measured.skipped.len(), 1);
        assert_eq!(measured.skipped[0].path, boundary);
        assert_eq!(
            measured.skipped[0].reason,
            if mount {
                SkipReason::MountPoint
            } else {
                SkipReason::DeviceBoundary
            }
        );
        let removed = remove_tree(&guard, "tree").unwrap();
        assert!(removed.errors.is_empty());
        assert_eq!(removed.skipped.len(), 1);
        assert!(!dir.path("tree/ordinary").exists());
        assert_eq!(
            fs::read(dir.path("tree/nested/boundary/keep")).unwrap(),
            b"outside"
        );
        assert!(dir.path("tree/nested").is_dir());
    }
}

#[test]
fn recovery_removes_staging_links_and_keeps_outside_data() {
    let dir = Directory::new();
    let outside = Directory::new();
    fs::write(outside.path("precious"), b"keep").unwrap();
    fs::create_dir_all(dir.path(".cpforge/staging")).unwrap();
    symlink_directory(&outside.0, &dir.path(".cpforge/staging/link"));
    #[cfg(unix)]
    let temp_link = {
        let temp_link = dir.path(format!(".cpf-tmp-{}", "a".repeat(32)));
        std::os::unix::fs::symlink(outside.path("precious"), &temp_link).unwrap();
        temp_link
    };
    let lock = dir.lock();
    let guard = lock.acquire("recover links").unwrap();
    assert!(
        guard.recovery_warning().is_none(),
        "{:?}",
        guard.recovery_warning()
    );
    assert!(!dir.path(".cpforge/staging/link").exists());
    assert_eq!(fs::read(outside.path("precious")).unwrap(), b"keep");
    #[cfg(unix)]
    assert!(
        fs::symlink_metadata(temp_link)
            .unwrap()
            .file_type()
            .is_symlink()
    );
}

#[cfg(target_os = "linux")]
#[test]
#[ignore = "requires CAP_SYS_ADMIN; run by the privileged Linux CI step"]
fn linux_bind_mount_boundary() {
    use cpforge::storage::SkipReason;

    struct Mount(PathBuf);
    impl Drop for Mount {
        fn drop(&mut self) {
            let result = Command::new("umount").arg(&self.0).status();
            if !std::thread::panicking() {
                assert!(result.unwrap().success());
            }
        }
    }

    let dir = Directory::new();
    let outside = Directory::new();
    fs::write(outside.path("keep"), b"outside").unwrap();
    let lock = dir.lock();
    let guard = lock.acquire("mount boundary").unwrap();
    guard.create_dir_all("tree/nested/mounted data").unwrap();
    fs::write(dir.path("tree/ordinary"), b"123").unwrap();
    let boundary = dir.path("tree/nested/mounted data");
    assert!(
        Command::new("mount")
            .arg("--bind")
            .arg(&outside.0)
            .arg(&boundary)
            .status()
            .unwrap()
            .success()
    );
    let mounted = Mount(boundary.clone());
    let measured = measure_tree(&guard, "tree").unwrap();
    assert_eq!(measured.bytes, 3);
    assert!(measured.errors.is_empty());
    assert_eq!(measured.skipped.len(), 1);
    assert_eq!(measured.skipped[0].path, boundary);
    assert_eq!(measured.skipped[0].reason, SkipReason::MountPoint);
    let removed = remove_tree(&guard, "tree").unwrap();
    assert!(removed.errors.is_empty());
    assert_eq!(removed.skipped.len(), 1);
    assert!(dir.path("tree/nested").is_dir());
    assert!(!dir.path("tree/ordinary").exists());
    assert_eq!(fs::read(outside.path("keep")).unwrap(), b"outside");
    drop(mounted);
    assert!(remove_tree(&guard, "tree").unwrap().errors.is_empty());
}

#[cfg(windows)]
#[test]
fn windows_read_only_replace_preserves_old_content() {
    let dir = Directory::new();
    fs::write(dir.path("record"), b"old").unwrap();
    let original = fs::metadata(dir.path("record")).unwrap().permissions();
    let mut read_only = original.clone();
    read_only.set_readonly(true);
    fs::set_permissions(dir.path("record"), read_only).unwrap();
    let lock = dir.lock();
    let guard = lock.acquire("read only replace").unwrap();
    let result = atomic_replace(&guard, "record", b"new", || Ok(()));
    fs::set_permissions(dir.path("record"), original).unwrap();
    let error = result.unwrap_err();
    assert!(!error.committed());
    assert!(matches!(error.kind, ErrorKind::Io { source, .. }
        if source.kind() == std::io::ErrorKind::PermissionDenied));
    assert_eq!(error.cleanup, Cleanup::Done);
    assert_eq!(fs::read(dir.path("record")).unwrap(), b"old");
    assert_no_temps(&dir.0);
}

#[cfg(feature = "storage-test-hooks")]
#[test]
fn temp_collision_retries_without_touching_existing_entries() {
    use cpforge::storage::TestHooks;
    for directory in [false, true] {
        let dir = Directory::new();
        let lock = dir.lock();
        let guard = lock
            .acquire_with_options(
                "collision",
                LockOptions {
                    test_hooks: TestHooks {
                        first_temp_random: Some([0; 16]),
                        ..TestHooks::default()
                    },
                    ..LockOptions::default()
                },
            )
            .unwrap();
        let occupied = dir.path(format!(".cpf-tmp-{}", "0".repeat(32)));
        if directory {
            fs::create_dir(&occupied).unwrap();
        } else {
            fs::write(&occupied, b"current temp").unwrap();
        }
        assert!(
            atomic_replace(&guard, "record", b"new", || Ok(()))
                .unwrap()
                .committed
        );
        assert!(
            publish_file_noclobber(&guard, "exclusive", b"complete")
                .unwrap()
                .committed
        );
        if directory {
            assert!(occupied.is_dir());
        } else {
            assert_eq!(fs::read(occupied).unwrap(), b"current temp");
        }
        assert_eq!(fs::read(dir.path("record")).unwrap(), b"new");
        assert_eq!(fs::read(dir.path("exclusive")).unwrap(), b"complete");
    }
}

#[cfg(feature = "storage-test-hooks")]
#[test]
fn disk_full_before_commit_keeps_old_file_and_cleans_temp() {
    use cpforge::storage::TestHooks;
    let dir = Directory::new();
    fs::write(dir.path("record"), b"old").unwrap();
    let lock = dir.lock();
    let guard = lock
        .acquire_with_options(
            "disk full",
            LockOptions {
                test_hooks: TestHooks {
                    fail_write_disk_full: true,
                    ..TestHooks::default()
                },
                ..LockOptions::default()
            },
        )
        .unwrap();
    let error = atomic_replace(&guard, "record", b"new", || Ok(())).unwrap_err();
    assert!(!error.committed());
    assert!(matches!(error.kind, ErrorKind::Io { .. }));
    assert_eq!(error.cleanup, Cleanup::Done);
    assert_eq!(fs::read(dir.path("record")).unwrap(), b"old");
    let error = publish_file_noclobber(&guard, "absent", b"new").unwrap_err();
    assert!(!error.committed());
    assert!(!dir.path("absent").exists());
    assert_no_temps(&dir.0);
}

#[test]
fn publication_races_an_external_exclusive_creator_without_overwriting_it() {
    let dir = Directory::new();
    let lock = dir.lock();
    let guard = lock.acquire("external race").unwrap();
    fs::write(dir.path("outsider-source"), b"outsider complete").unwrap();
    for attempt in 0..16 {
        let target = dir.path(format!("race-{attempt}"));
        let source = dir.path("outsider-source");
        let barrier = Arc::new(Barrier::new(2));
        let child_barrier = Arc::clone(&barrier);
        let child_target = target.clone();
        let creator = std::thread::spawn(move || {
            child_barrier.wait();
            // A complete native no-clobber publication by a non-cooperating
            // creator, which intentionally does not take the workspace lock.
            match fs::hard_link(source, child_target) {
                Ok(()) => true,
                Err(error) => {
                    assert_eq!(error.kind(), std::io::ErrorKind::AlreadyExists);
                    false
                }
            }
        });
        barrier.wait();
        let committed = match publish_file_noclobber(&guard, &target, b"cpf complete") {
            Ok(published) => published.committed,
            Err(error) => {
                assert!(matches!(error.kind, ErrorKind::Exists { .. }));
                false
            }
        };
        assert_eq!(
            usize::from(committed) + usize::from(creator.join().unwrap()),
            1
        );
        assert_eq!(
            fs::read(target).unwrap(),
            if committed {
                b"cpf complete".as_slice()
            } else {
                b"outsider complete".as_slice()
            }
        );
    }
    assert_no_temps(&dir.0);
}

#[test]
fn removed_created_directories_do_not_taint_later_publication_durability() {
    let dir = Directory::new();
    let lock = dir.lock();
    let guard = lock.acquire("removed ancestors").unwrap();
    guard.create_dir_all("unused/nested").unwrap();
    fs::remove_dir_all(dir.path("unused")).unwrap();
    for published in [
        atomic_replace(&guard, "record", b"new", || Ok(())).unwrap(),
        publish_file_noclobber(&guard, "exclusive", b"new").unwrap(),
    ] {
        assert!(
            matches!(published.durability, cpforge::storage::Durability::Ok),
            "{published:?}"
        );
    }
}

#[test]
fn acquisition_rejects_linked_metadata_directory_and_lock_entry() {
    for metadata_link in [true, false] {
        let dir = Directory::new();
        let outside = Directory::new();
        if metadata_link {
            symlink_directory(&outside.0, &dir.path(".cpforge"));
        } else {
            fs::create_dir(dir.path(".cpforge")).unwrap();
            #[cfg(unix)]
            {
                fs::write(outside.path("lock"), b"keep").unwrap();
                std::os::unix::fs::symlink(outside.path("lock"), dir.path(".cpforge/lock"))
                    .unwrap();
            }
            #[cfg(windows)]
            symlink_directory(&outside.0, &dir.path(".cpforge/lock"));
        }
        let lock = dir.lock();
        assert!(matches!(
            lock.acquire("linked lock").unwrap_err().kind,
            ErrorKind::LinkInPath { .. }
        ));
        if metadata_link {
            assert!(!outside.path("lock").exists());
        }
    }
}

#[test]
fn cached_lock_is_reopened_when_the_lock_entry_is_replaced() {
    let dir = Directory::new();
    let lock = dir.lock();
    drop(lock.acquire("initial lock").unwrap());
    fs::rename(dir.path(".cpforge/lock"), dir.path(".cpforge/old-lock")).unwrap();
    File::create(dir.path(".cpforge/lock")).unwrap();
    let guard = lock.acquire("replacement lock").unwrap();
    let output = Command::new(std::env::current_exe().unwrap())
        .args(["--ignored", "--exact", "lock_child", "--nocapture"])
        .env("CPF_STORAGE_LOCK_ROOT", &dir.0)
        .env("CPF_STORAGE_LOCK_BUSY", "yes")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    drop(guard);
}

#[test]
fn recovery_skips_linked_user_metadata_locations_without_warning() {
    let dir = Directory::new();
    let outside = Directory::new();
    let exact = format!(".cpf-tmp-{}", "a".repeat(32));
    fs::write(outside.path(&exact), b"keep").unwrap();
    fs::create_dir(dir.path("problems")).unwrap();
    symlink_directory(&outside.0, &dir.path("problems/linked"));
    let lock = dir.lock();
    let guard = lock.acquire("linked recovery locations").unwrap();
    assert!(
        guard.recovery_warning().is_none(),
        "{:?}",
        guard.recovery_warning()
    );
    assert_eq!(fs::read(outside.path(exact)).unwrap(), b"keep");
}

#[test]
fn walker_handles_deep_trees_without_path_or_handle_depth_limits() {
    let dir = Directory::new();
    let lock = dir.lock();
    let guard = lock.acquire("deep tree").unwrap();
    let root = guard.create_dir_all("deep").unwrap();
    #[cfg(unix)]
    {
        use std::os::fd::{AsRawFd, FromRawFd};
        // More than PATH_MAX bytes on Linux and macOS, and more levels than
        // macOS's usual descriptor limit. Fixture creation is also relative.
        let mut parent = File::open(root).unwrap();
        let name = c"sixteen-byte-dir";
        for _ in 0..350 {
            // SAFETY: parent is live and the component is terminated.
            assert_eq!(
                unsafe { libc::mkdirat(parent.as_raw_fd(), name.as_ptr(), 0o700) },
                0
            );
            // SAFETY: openat receives a live parent, terminated name and no O_CREAT.
            let fd = unsafe {
                libc::openat(
                    parent.as_raw_fd(),
                    name.as_ptr(),
                    libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                )
            };
            assert!(fd >= 0, "{}", std::io::Error::last_os_error());
            // SAFETY: successful openat returned a newly owned descriptor.
            parent = unsafe { File::from_raw_fd(fd) };
        }
        // SAFETY: parent and the terminated name are valid; mode accompanies O_CREAT.
        let fd = unsafe {
            libc::openat(
                parent.as_raw_fd(),
                c"data".as_ptr(),
                libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_CLOEXEC,
                0o600,
            )
        };
        assert!(fd >= 0, "{}", std::io::Error::last_os_error());
        // SAFETY: successful openat returned a newly owned descriptor.
        let mut file = unsafe { File::from_raw_fd(fd) };
        file.write_all(b"deep data").unwrap();
    }
    #[cfg(windows)]
    {
        let mut path = root;
        for _ in 0..350 {
            path.push("nested");
            fs::create_dir(&path).unwrap();
        }
        fs::write(path.join("data"), b"deep data").unwrap();
    }
    let measured = measure_tree(&guard, "deep").unwrap();
    assert!(measured.errors.is_empty(), "{measured:?}");
    assert!(measured.skipped.is_empty());
    assert_eq!(measured.bytes, 9);
    let removed = remove_tree(&guard, "deep").unwrap();
    assert!(removed.errors.is_empty(), "{removed:?}");
    assert!(removed.skipped.is_empty());
    assert_eq!(removed.bytes, 9);
    assert!(!dir.path("deep").exists());
}

#[test]
fn walker_removes_read_only_disposable_entries() {
    let dir = Directory::new();
    let outside = Directory::new();
    fs::write(outside.path("keep"), b"outside").unwrap();
    let lock = dir.lock();
    let guard = lock.acquire("read only tree").unwrap();
    let tree = guard.create_dir_all("tree/nested").unwrap();
    fs::write(tree.join("data"), b"owned").unwrap();
    symlink_directory(&outside.0, &tree.join("link"));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&tree, fs::Permissions::from_mode(0o500)).unwrap();
    }
    #[cfg(windows)]
    {
        let mut permissions = fs::metadata(tree.join("data")).unwrap().permissions();
        permissions.set_readonly(true);
        fs::set_permissions(tree.join("data"), permissions).unwrap();
    }
    let removed = remove_tree(&guard, "tree").unwrap();
    assert!(removed.errors.is_empty(), "{removed:?}");
    assert!(removed.skipped.is_empty());
    assert!(!dir.path("tree").exists());
    assert_eq!(fs::read(outside.path("keep")).unwrap(), b"outside");
}

#[cfg(windows)]
#[test]
fn windows_forward_slash_relative_paths_work_through_storage_primitives() {
    let dir = Directory::new();
    fs::create_dir_all(dir.path(".cpforge/staging/old/sub")).unwrap();
    fs::write(dir.path(".cpforge/staging/old/sub/data"), b"leftover").unwrap();
    fs::create_dir_all(dir.path(".cpforge/facts/problem")).unwrap();
    let leftover = dir.path(format!(
        ".cpforge/facts/problem/.cpf-tmp-{}",
        "a".repeat(32)
    ));
    fs::write(&leftover, b"leftover").unwrap();
    let lock = dir.lock();
    let guard = lock.acquire("forward slash paths").unwrap();
    let lock_path = guard.check_path(".cpforge/lock").unwrap();
    assert!(!lock_path.as_os_str().to_string_lossy().contains('/'));
    assert!(lock_path.is_file());
    assert!(
        guard.recovery_warning().is_none(),
        "{:?}",
        guard.recovery_warning()
    );
    assert!(!leftover.exists());
    assert!(!dir.path(".cpforge/staging/old").exists());
    guard.create_dir_all("records/nested").unwrap();
    atomic_replace(&guard, "records/nested/replace", b"replacement", || Ok(())).unwrap();
    publish_file_noclobber(&guard, "records/nested/exclusive", b"exclusive").unwrap();
    let stage = guard.create_dir_all(".cpforge/staging/new/sub").unwrap();
    fs::write(stage.join("data"), b"staged").unwrap();
    publish_dir_noclobber(&guard, ".cpforge/staging/new", "records/nested/directory").unwrap();
    assert_eq!(
        fs::read(dir.path("records/nested/directory/sub/data")).unwrap(),
        b"staged"
    );
    assert_eq!(
        measure_tree(&guard, "records/nested/directory")
            .unwrap()
            .bytes,
        6
    );
    assert!(
        remove_tree(&guard, "records/nested/directory")
            .unwrap()
            .errors
            .is_empty()
    );
    assert_eq!(
        fs::read(dir.path("records/nested/replace")).unwrap(),
        b"replacement"
    );
    assert_eq!(
        fs::read(dir.path("records/nested/exclusive")).unwrap(),
        b"exclusive"
    );
}

#[cfg(windows)]
#[test]
fn windows_directory_publication_retries_open_staged_files_and_retains_on_timeout() {
    use std::os::windows::fs::OpenOptionsExt;
    use windows_sys::Win32::Storage::FileSystem::{FILE_SHARE_READ, FILE_SHARE_WRITE};
    let dir = Directory::new();
    let lock = dir.lock();
    let guard = lock.acquire("open staged file").unwrap();
    let stage = guard.create_dir_all(".cpforge/staging/tx").unwrap();
    fs::write(stage.join("data"), b"staged").unwrap();
    let held = fs::OpenOptions::new()
        .read(true)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
        .open(stage.join("data"))
        .unwrap();
    let start = Instant::now();
    let error = publish_dir_noclobber(&guard, &stage, "published").unwrap_err();
    assert!(start.elapsed() >= Duration::from_secs(2));
    assert!(start.elapsed() < Duration::from_secs(5));
    assert!(matches!(error.kind, ErrorKind::Io { .. }));
    assert_eq!(error.cleanup, Cleanup::Pending(stage.clone()));
    assert_eq!(fs::read(stage.join("data")).unwrap(), b"staged");
    assert!(!dir.path("published").exists());
    // Retry that same staged tree while a blocker is released within the bound.
    let release = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(150));
        drop(held);
    });
    let published = publish_dir_noclobber(&guard, &stage, "published");
    release.join().unwrap();
    assert!(published.unwrap().committed);
    assert!(!stage.exists());
    assert_eq!(fs::read(dir.path("published/data")).unwrap(), b"staged");
}

#[cfg(windows)]
#[test]
fn windows_exclusive_file_publication_keeps_an_open_existing_destination() {
    use std::os::windows::fs::OpenOptionsExt;
    use windows_sys::Win32::Storage::FileSystem::{FILE_SHARE_READ, FILE_SHARE_WRITE};
    let dir = Directory::new();
    fs::write(dir.path("record"), b"old").unwrap();
    let held = fs::OpenOptions::new()
        .read(true)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
        .open(dir.path("record"))
        .unwrap();
    let lock = dir.lock();
    let guard = lock.acquire("open exclusive destination").unwrap();
    let error = publish_file_noclobber(&guard, "record", b"new").unwrap_err();
    assert!(matches!(error.kind, ErrorKind::Exists { .. }));
    assert_eq!(error.cleanup, Cleanup::Done);
    assert_eq!(fs::read(dir.path("record")).unwrap(), b"old");
    assert_no_temps(&dir.0);
    drop(held);
}
