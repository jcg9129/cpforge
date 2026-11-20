//! Real process-abort checks for primitive and shared-record commit boundaries.
#![cfg(feature = "storage-test-hooks")]

mod storage_support;

use std::fs;
use std::process::{Command, Output};

use cpforge::storage::{
    ErrorKind, LockOptions, TestHooks, WorkspaceLock, atomic_replace, edit_toml,
    publish_dir_noclobber, publish_file_noclobber,
};
use storage_support::{Directory, assert_no_temps};

const OLD: &[u8] = b"schema = 1\ncount = 0\n";
const NEW: &[u8] = b"schema = 1\ncount = 1\n";

fn assert_aborted(output: &Output, point: &str) {
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr
            .lines()
            .any(|line| line == format!("CPF-CRASH-AT {point}")),
        "{stderr}"
    );
    assert!(!output.status.success());
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        assert_eq!(output.status.signal(), Some(libc::SIGABRT), "{output:?}");
    }
    #[cfg(windows)]
    {
        // Rust aborts with STATUS_FATAL_APP_EXIT or the CRT's abort exit code.
        let code = output.status.code().expect("Windows process exit code") as u32;
        assert!(
            code == 3 || code == 0x4000_0015 || code >= 0x8000_0000,
            "{output:?}"
        );
    }
}

#[test]
fn every_commit_boundary_has_only_complete_allowed_states_and_recovers() {
    let mut points = vec![
        ("atomic_replace.temp_synced", false),
        ("atomic_replace.renamed", true),
        ("atomic_replace.directory_synced", true),
        ("file_noclobber.before_publish", false),
        ("file_noclobber.after_publish", true),
        ("file_noclobber.directory_synced", true),
        ("dir_noclobber.staging_synced", false),
        ("dir_noclobber.renamed", true),
        ("dir_noclobber.directories_synced", true),
        ("edit_toml.validated", false),
        ("edit_toml.published", true),
        ("lock.acquired", false),
        ("lock.recovered", false),
    ];
    if cfg!(target_os = "linux") {
        points.push(("file_noclobber.linked", true));
    }
    for (point, committed) in points {
        let dir = Directory::new();
        fs::write(dir.path("record.toml"), OLD).unwrap();
        let leftover = dir.path(format!(".cpf-tmp-{}", "a".repeat(32)));
        fs::write(&leftover, b"leftover").unwrap();
        fs::create_dir_all(dir.path(".cpforge/staging/old")).unwrap();
        fs::write(dir.path(".cpforge/staging/old/data"), b"leftover").unwrap();
        let output = Command::new(std::env::current_exe().unwrap())
            .args(["--ignored", "--exact", "crash_child", "--nocapture"])
            .env("CPF_STORAGE_HELPER_ROOT", &dir.0)
            .env("CPF_STORAGE_CRASH_AT", point)
            .output()
            .unwrap();
        assert_aborted(&output, point);
        assert_eq!(leftover.exists(), point == "lock.acquired", "{point}");
        assert_eq!(
            dir.path(".cpforge/staging/old").exists(),
            point == "lock.acquired",
            "{point}"
        );
        let operation = point.split_once('.').unwrap().0;
        match operation {
            "atomic_replace" | "edit_toml" => {
                assert_eq!(
                    fs::read(dir.path("record.toml")).unwrap(),
                    if committed { NEW } else { OLD },
                    "{point}"
                );
                if !committed {
                    assert!(fs::read_dir(&dir.0).unwrap().any(|entry| {
                        entry
                            .unwrap()
                            .file_name()
                            .to_string_lossy()
                            .starts_with(".cpf-tmp-")
                    }));
                }
            }
            "file_noclobber" => {
                assert_eq!(dir.path("exclusive").exists(), committed, "{point}");
                if committed {
                    assert_eq!(fs::read(dir.path("exclusive")).unwrap(), NEW);
                }
                if matches!(
                    point,
                    "file_noclobber.before_publish" | "file_noclobber.linked"
                ) {
                    assert!(fs::read_dir(&dir.0).unwrap().any(|entry| {
                        entry
                            .unwrap()
                            .file_name()
                            .to_string_lossy()
                            .starts_with(".cpf-tmp-")
                    }));
                }
            }
            "dir_noclobber" => {
                assert_eq!(dir.path("published").exists(), committed, "{point}");
                assert_eq!(
                    dir.path(".cpforge/staging/current").exists(),
                    !committed,
                    "{point}"
                );
                let data = if committed {
                    "published/nested/data"
                } else {
                    ".cpforge/staging/current/nested/data"
                };
                assert_eq!(fs::read(dir.path(data)).unwrap(), NEW);
            }
            "lock" => assert_eq!(fs::read(dir.path("record.toml")).unwrap(), OLD),
            _ => unreachable!(),
        }
        // A new acquisition proves OS-lock release on process death and recovery
        // before the rerun can create any new transaction artifacts.
        let lock = dir.lock();
        let guard = lock.acquire("recover and rerun").unwrap();
        assert!(guard.recovery_warning().is_none());
        assert_no_temps(&dir.0);
        assert!(
            fs::read_dir(dir.path(".cpforge/staging"))
                .unwrap()
                .next()
                .is_none()
        );
        match operation {
            "atomic_replace" | "lock" => {
                assert!(
                    atomic_replace(&guard, "record.toml", NEW, || Ok(()))
                        .unwrap()
                        .committed
                );
            }
            "edit_toml" => {
                let published = edit_toml(
                    &guard,
                    "record.toml",
                    |_| Ok::<_, ()>(()),
                    |doc| {
                        doc["count"] = toml_edit::value(1);
                        Ok(())
                    },
                )
                .unwrap();
                assert_eq!(published.committed, !committed);
                assert_eq!(fs::read(dir.path("record.toml")).unwrap(), NEW);
            }
            "file_noclobber" => {
                let result = publish_file_noclobber(&guard, "exclusive", NEW);
                if committed {
                    assert!(matches!(result.unwrap_err().kind, ErrorKind::Exists { .. }));
                } else {
                    assert!(result.unwrap().committed);
                }
                assert_eq!(fs::read(dir.path("exclusive")).unwrap(), NEW);
            }
            "dir_noclobber" => {
                let stage = guard
                    .create_dir_all(".cpforge/staging/rerun/nested")
                    .unwrap();
                fs::write(stage.join("data"), NEW).unwrap();
                let result = publish_dir_noclobber(&guard, ".cpforge/staging/rerun", "published");
                if committed {
                    assert!(matches!(result.unwrap_err().kind, ErrorKind::Exists { .. }));
                } else {
                    assert!(result.unwrap().committed);
                }
                assert_eq!(fs::read(dir.path("published/nested/data")).unwrap(), NEW);
            }
            _ => unreachable!(),
        }
    }
}

#[test]
fn crash_occurrences_are_one_based_and_process_local() {
    for point in [
        "atomic_replace.temp_synced",
        "lock.acquired",
        "edit_toml.validated",
    ] {
        // Repeating a fresh process also verifies the counters never leak across processes.
        for _ in 0..2 {
            let dir = Directory::new();
            fs::write(dir.path("record.toml"), OLD).unwrap();
            let output = Command::new(std::env::current_exe().unwrap())
                .args(["--ignored", "--exact", "crash_child", "--nocapture"])
                .env("CPF_STORAGE_HELPER_ROOT", &dir.0)
                .env("CPF_STORAGE_CRASH_AT", format!("{point}#2"))
                .output()
                .unwrap();
            assert_aborted(&output, point);
            assert_eq!(fs::read(dir.path("record.toml")).unwrap(), NEW);
            let lock = dir.lock();
            let guard = lock.acquire("recover second attempt").unwrap();
            assert!(guard.recovery_warning().is_none());
            assert_no_temps(&dir.0);
            atomic_replace(&guard, "record.toml", NEW, || Ok(())).unwrap();
        }
    }
}

#[test]
#[ignore]
fn crash_child() {
    let Some(root) = std::env::var_os("CPF_STORAGE_HELPER_ROOT") else {
        return;
    };
    #[cfg(unix)]
    {
        // Process-crash tests need no core file (which can be very large).
        let limit = libc::rlimit {
            rlim_cur: 0,
            rlim_max: 0,
        };
        // SAFETY: limit is valid and only this helper's core limit is changed.
        assert_eq!(unsafe { libc::setrlimit(libc::RLIMIT_CORE, &limit) }, 0);
    }
    #[cfg(windows)]
    {
        // Suppress the OS crash dialog in unattended test runs.
        // SAFETY: changes only this helper process's error-reporting mode.
        unsafe {
            windows_sys::Win32::System::Diagnostics::Debug::SetErrorMode(
                windows_sys::Win32::System::Diagnostics::Debug::SEM_NOGPFAULTERRORBOX,
            );
        }
    }
    let selected = std::env::var("CPF_STORAGE_CRASH_AT").unwrap();
    let point = selected.split('#').next().unwrap();
    let lock = WorkspaceLock::new(root).unwrap();
    for _ in 0..2 {
        let guard = lock
            .acquire_with_options(
                "crash operation",
                LockOptions {
                    test_hooks: TestHooks {
                        force_link_fallback: point == "file_noclobber.linked",
                        ..TestHooks::default()
                    },
                    ..LockOptions::default()
                },
            )
            .unwrap();
        match point.split_once('.').unwrap().0 {
            "atomic_replace" => {
                atomic_replace(&guard, "record.toml", NEW, || Ok(())).unwrap();
            }
            "file_noclobber" => {
                publish_file_noclobber(&guard, "exclusive", NEW).unwrap();
            }
            "dir_noclobber" => {
                let stage = guard
                    .create_dir_all(".cpforge/staging/current/nested")
                    .unwrap();
                fs::write(stage.join("data"), NEW).unwrap();
                publish_dir_noclobber(&guard, ".cpforge/staging/current", "published").unwrap();
            }
            "edit_toml" => {
                edit_toml(
                    &guard,
                    "record.toml",
                    |_| Ok::<_, ()>(()),
                    |doc| {
                        // Each iteration must change the document so an nth
                        // crash point still reaches an actual publication.
                        let count = doc["count"].as_integer().unwrap();
                        doc["count"] = toml_edit::value(count + 1);
                        Ok(())
                    },
                )
                .unwrap();
            }
            "lock" => {
                atomic_replace(&guard, "record.toml", NEW, || Ok(())).unwrap();
            }
            _ => panic!("unknown crash operation"),
        }
    }
    panic!("crash point was not reached");
}
