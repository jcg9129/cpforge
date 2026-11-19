use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use super::{RecoveryIncomplete, WorkspaceGuard, remove_tree, sys};

pub(crate) fn recover(guard: &WorkspaceGuard<'_>) -> Option<RecoveryIncomplete> {
    let mut warning = RecoveryIncomplete::default();
    scan_temps(guard, guard.root(), &mut warning);
    scan_temps(guard, &guard.root().join(".cpforge"), &mut warning);
    for (parent, tests) in [
        ("problems", true),
        ("contests", false),
        (".cpforge/facts", false),
    ] {
        let parent = guard.root().join(parent);
        for child in entries(guard, &parent, &mut warning) {
            match fs::symlink_metadata(&child) {
                // Linked user locations are not recovery trees we own.
                Ok(metadata) if sys::is_link(&metadata) => {}
                Ok(metadata) if metadata.is_dir() => {
                    scan_temps(guard, &child, &mut warning);
                    if tests {
                        scan_temps(guard, &child.join("tests"), &mut warning);
                    }
                }
                Ok(_) => {}
                Err(error) => warning.push(child, error),
            }
        }
    }
    let staging = guard.root().join(".cpforge/staging");
    for entry in entries(guard, &staging, &mut warning) {
        if fail_remove(guard, &entry) {
            warning.push(
                entry,
                io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "injected recovery removal failure",
                ),
            );
            continue;
        }
        match remove_tree(guard, &entry) {
            Ok(report) => {
                for (path, error) in report.errors {
                    warning.push(path, error);
                }
                for skipped in report.skipped {
                    warning.push(
                        skipped.path,
                        io::Error::other("recovery skipped mount boundary"),
                    );
                }
            }
            Err(error) => warning.push(entry, io::Error::other(error)),
        }
    }
    if warning.paths.is_empty() {
        None
    } else {
        Some(warning)
    }
}

fn entries(
    guard: &WorkspaceGuard<'_>,
    location: &Path,
    warning: &mut RecoveryIncomplete,
) -> Vec<PathBuf> {
    if let Err(error) = guard.check_path(location) {
        warning.push(location.into(), io::Error::other(error));
        return Vec::new();
    }
    #[cfg(feature = "storage-test-hooks")]
    if guard.test_hooks.recovery_read_failure.as_deref() == Some(location) {
        warning.push(
            location.into(),
            io::Error::new(
                io::ErrorKind::PermissionDenied,
                "injected recovery enumeration failure",
            ),
        );
        return Vec::new();
    }
    match fs::read_dir(location) {
        Ok(entries) => entries
            .filter_map(|entry| match entry {
                Ok(entry) => Some(entry.path()),
                Err(error) => {
                    warning.push(location.into(), error);
                    None
                }
            })
            .collect(),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Vec::new(),
        Err(error) => {
            warning.push(location.into(), error);
            Vec::new()
        }
    }
}

fn scan_temps(guard: &WorkspaceGuard<'_>, location: &Path, warning: &mut RecoveryIncomplete) {
    for path in entries(guard, location, warning) {
        if !path
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(exact_temp_name)
        {
            continue;
        }
        match fs::symlink_metadata(&path) {
            Ok(metadata) if metadata.is_file() && !sys::is_link(&metadata) => {
                let result = if fail_remove(guard, &path) {
                    Err(io::Error::new(
                        io::ErrorKind::PermissionDenied,
                        "injected recovery removal failure",
                    ))
                } else {
                    fs::remove_file(&path)
                };
                if let Err(error) = result {
                    warning.push(path, error);
                }
            }
            Ok(_) => {}
            Err(error) => warning.push(path, error),
        }
    }
}

fn fail_remove(guard: &WorkspaceGuard<'_>, path: &Path) -> bool {
    #[cfg(feature = "storage-test-hooks")]
    {
        guard.test_hooks.recovery_remove_failure.as_deref() == Some(path)
    }
    #[cfg(not(feature = "storage-test-hooks"))]
    {
        let _ = (guard, path);
        false
    }
}

pub(crate) fn exact_temp_name(name: &str) -> bool {
    name.strip_prefix(".cpf-tmp-").is_some_and(|suffix| {
        suffix.len() == 32
            && suffix
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    })
}
