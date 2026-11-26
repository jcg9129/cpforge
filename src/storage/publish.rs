use std::fs::{self, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

use super::{
    Cleanup, Conflict, Durability, ErrorKind, Published, StorageError, WorkspaceGuard, sys, walk,
};

/// Atomically replace a file. The validation hook runs immediately before every
/// publishing attempt, including each Windows sharing/access-denied retry.
/// This is a validation hook, not compare-and-swap with other file editors.
pub fn atomic_replace(
    guard: &WorkspaceGuard<'_>,
    target: impl AsRef<Path>,
    bytes: &[u8],
    validate: impl FnMut() -> Result<(), Conflict>,
) -> Result<Published, StorageError> {
    replace_with_commit(guard, target.as_ref(), bytes, validate, || {})
}

/// Internal commit notification runs after rename and before directory sync.
pub(crate) fn replace_with_commit(
    guard: &WorkspaceGuard<'_>,
    target: &Path,
    mut bytes: &[u8],
    mut validate: impl FnMut() -> Result<(), Conflict>,
    committed: impl FnOnce(),
) -> Result<Published, StorageError> {
    let target = destination(guard, target)?;
    let temp = prepare_temp(guard, &target, &mut bytes, true)?;
    super::crash::point("atomic_replace.temp_synced");
    let result = replace_attempts(guard, &temp, &target, &mut validate);
    if let Err(error) = result {
        return Err(clean_file(guard, error, &temp));
    }
    committed();
    super::crash::point("atomic_replace.renamed");
    let published = finish(
        guard,
        &target,
        target.parent().expect("destination parent"),
        Cleanup::Done,
    );
    super::crash::point("atomic_replace.directory_synced");
    Ok(published)
}

/// Create a complete file exclusively. An existing destination is never reused.
pub fn publish_file_noclobber(
    guard: &WorkspaceGuard<'_>,
    target: impl AsRef<Path>,
    mut bytes: &[u8],
) -> Result<Published, StorageError> {
    publish_file_noclobber_from_reader(guard, target, &mut bytes)
}

/// Streaming form of [`publish_file_noclobber`], with the same commit, cleanup
/// and durability semantics. Read failures occur before the publication attempt.
pub fn publish_file_noclobber_from_reader(
    guard: &WorkspaceGuard<'_>,
    target: impl AsRef<Path>,
    reader: &mut impl Read,
) -> Result<Published, StorageError> {
    publish_reader(guard, target.as_ref(), reader, false)
}

/// Notes use the same no-clobber protocol with named test boundaries.
pub(crate) fn publish_notes(
    guard: &WorkspaceGuard<'_>,
    target: &Path,
) -> Result<Published, StorageError> {
    publish_reader(guard, target, &mut &b""[..], true)
}

fn publish_reader(
    guard: &WorkspaceGuard<'_>,
    target: &Path,
    reader: &mut impl Read,
    notes: bool,
) -> Result<Published, StorageError> {
    let target = destination(guard, target)?;
    let temp = prepare_temp(guard, &target, reader, false)?;
    if notes {
        super::crash::barrier("notes.before_publish");
        super::crash::point("notes.before_publish");
    }
    #[cfg(feature = "storage-test-hooks")]
    if let Some(hook) = guard.test_hooks.before_file_publish {
        hook(&target);
    }
    super::crash::point("file_noclobber.before_publish");
    #[cfg(feature = "storage-test-hooks")]
    let result = if guard.test_hooks.fail_publish_permission_denied {
        Err(io::Error::from(io::ErrorKind::PermissionDenied))
    } else if guard.test_hooks.force_cross_filesystem {
        Err(io::Error::from_raw_os_error(cross_device_code()))
    } else if guard.test_hooks.force_link_fallback || crash_link_fallback() {
        Err(io::Error::from_raw_os_error(unsupported_code()))
    } else {
        exclusive_attempts(&temp, &target)
    };
    #[cfg(not(feature = "storage-test-hooks"))]
    let result = exclusive_attempts(&temp, &target);
    let cleanup = match result {
        Ok(()) => Cleanup::Done,
        #[cfg(target_os = "linux")]
        Err(error) if filesystem_refuses_noreplace(&error) => {
            // Only a native refusal of RENAME_NOREPLACE permits this fallback.
            if let Err(error) = fs::hard_link(&temp, &target) {
                return Err(clean_file(
                    guard,
                    publication_error("publish file", &temp, &target, error),
                    &temp,
                ));
            }
            super::crash::point("file_noclobber.linked");
            // link is the commit point. An unlink failure is only a warning.
            unlink_temp(guard, &temp)
        }
        Err(error) => {
            return Err(clean_file(
                guard,
                publication_error("publish file", &temp, &target, error),
                &temp,
            ));
        }
    };
    if notes {
        super::crash::point("notes.published");
    }
    super::crash::point("file_noclobber.after_publish");
    let published = finish(guard, &target, temp.parent().expect("temp parent"), cleanup);
    super::crash::point("file_noclobber.directory_synced");
    Ok(published)
}

#[cfg(feature = "storage-test-hooks")]
fn crash_link_fallback() -> bool {
    #[cfg(target_os = "linux")]
    {
        super::crash::force_link_fallback()
    }
    #[cfg(not(target_os = "linux"))]
    {
        false
    }
}

/// Publish a fully staged tree without clobbering even an empty directory.
/// The source must be strictly below `.cpforge/staging/`. All pre-commit
/// failures retain staging and report Pending; sources outside staging remain
/// caller-owned. Recovery removes retained staging at the next acquisition.
pub fn publish_dir_noclobber(
    guard: &WorkspaceGuard<'_>,
    source: impl AsRef<Path>,
    target: impl AsRef<Path>,
) -> Result<Published, StorageError> {
    let staging = guard.root().join(".cpforge/staging");
    let source = guard.root().join(source.as_ref());
    // Establish lexical ownership before any filesystem check or cleanup.
    if !source.strip_prefix(&staging).is_ok_and(|relative| {
        relative.components().next().is_some()
            && relative
                .components()
                .all(|component| matches!(component, std::path::Component::Normal(_)))
    }) {
        return Err(StorageError::new(ErrorKind::InvalidPath { path: source }));
    }
    let source = guard.check_path(&source).map_err(|mut error| {
        error.cleanup = Cleanup::Pending(source.clone());
        error
    })?;
    let target_path = guard.root().join(target.as_ref());
    let checked_target = destination(guard, target.as_ref());
    // An overlapping target may contain existing user data inside the source.
    // Refuse before any error can trigger disposal of that tree.
    if target_path.starts_with(&source) || source.starts_with(&target_path) {
        let mut error = StorageError::new(ErrorKind::InvalidPath { path: target_path });
        error.cleanup = Cleanup::Pending(source);
        return Err(error);
    }
    let result = (|| {
        let target = checked_target?;
        if !fs::symlink_metadata(&source)
            .map_err(|e| StorageError::io(&source, e))?
            .is_dir()
        {
            return Err(StorageError::new(ErrorKind::InvalidPath {
                path: source.clone(),
            }));
        }
        pre_sync(guard).map_err(|e| StorageError::io(&source, e))?;
        walk::sync_tree(guard, &source)?;
        super::crash::point("dir_noclobber.staging_synced");
        #[cfg(feature = "storage-test-hooks")]
        let published = if guard.test_hooks.fail_publish_permission_denied {
            Err(io::Error::from(io::ErrorKind::PermissionDenied))
        } else if guard.test_hooks.force_directory_unsupported {
            Err(io::Error::from_raw_os_error(unsupported_code()))
        } else if guard.test_hooks.force_cross_filesystem {
            Err(io::Error::from_raw_os_error(cross_device_code()))
        } else {
            exclusive_attempts(&source, &target)
        };
        #[cfg(not(feature = "storage-test-hooks"))]
        let published = exclusive_attempts(&source, &target);
        published
            .map_err(|error| publication_error("publish directory", &source, &target, error))?;
        super::crash::point("dir_noclobber.renamed");
        guard.created.borrow_mut().retain_mut(|created| {
            if created.as_path() == source.as_path() {
                *created = target.clone();
                true
            } else {
                // The staged descendants were already synced bottom-up.
                !created.starts_with(&source)
            }
        });
        let published = finish(
            guard,
            &target,
            source.parent().expect("staging parent"),
            Cleanup::Done,
        );
        super::crash::point("dir_noclobber.directories_synced");
        Ok(published)
    })();
    result.map_err(|mut error: StorageError| {
        error.cleanup = Cleanup::Pending(source);
        error
    })
}

fn destination(guard: &WorkspaceGuard<'_>, target: &Path) -> Result<PathBuf, StorageError> {
    let target = guard.check_path(target)?;
    let name = target
        .file_name()
        .filter(|_| target != guard.root())
        .ok_or_else(|| {
            StorageError::new(ErrorKind::InvalidPath {
                path: target.clone(),
            })
        })?;
    crate::names::PortableComponent::from_os_str(name)
        .map_err(|e| StorageError::new(ErrorKind::InvalidName(e)))?;
    Ok(target)
}

fn prepare_temp(
    guard: &WorkspaceGuard<'_>,
    target: &Path,
    reader: &mut impl Read,
    copy_mode: bool,
) -> Result<PathBuf, StorageError> {
    let parent = target.parent().expect("destination parent");
    guard.check_path(parent)?;
    #[cfg(feature = "storage-test-hooks")]
    let mut first_random = guard.test_hooks.first_temp_random;
    let (temp, mut file) = loop {
        let mut random = [0_u8; 16];
        #[cfg(feature = "storage-test-hooks")]
        let injected = first_random.take();
        #[cfg(not(feature = "storage-test-hooks"))]
        let injected: Option<[u8; 16]> = None;
        if let Some(bits) = injected {
            random = bits;
        } else {
            getrandom::fill(&mut random)
                .map_err(|e| StorageError::io(parent, io::Error::other(e.to_string())))?;
        }
        use std::fmt::Write as _;
        let mut name = String::from(".cpf-tmp-");
        for byte in random {
            write!(name, "{byte:02x}").expect("String formatting");
        }
        debug_assert!(super::recovery::exact_temp_name(&name));
        let temp = parent.join(name);
        match OpenOptions::new().write(true).create_new(true).open(&temp) {
            Ok(file) => break (temp, file),
            Err(error)
                if error.kind() == io::ErrorKind::AlreadyExists
                    || fs::symlink_metadata(&temp).is_ok() =>
            {
                continue;
            }
            Err(error) => return Err(StorageError::io(&temp, error)),
        }
    };
    let prepared = (|| {
        #[cfg(feature = "storage-test-hooks")]
        if guard.test_hooks.fail_write_disk_full {
            #[cfg(unix)]
            let code = libc::ENOSPC;
            #[cfg(windows)]
            let code = windows_sys::Win32::Foundation::ERROR_DISK_FULL as i32;
            return Err(StorageError::io(&temp, io::Error::from_raw_os_error(code)));
        }
        #[cfg(unix)]
        let permissions = if copy_mode {
            guard.check_path(target)?;
            match fs::symlink_metadata(target) {
                Ok(metadata) => {
                    if !metadata.is_file() {
                        return Err(StorageError::new(ErrorKind::InvalidPath {
                            path: target.into(),
                        }));
                    }
                    Some(metadata.permissions())
                }
                Err(error) if error.kind() == io::ErrorKind::NotFound => None,
                Err(error) => return Err(StorageError::io(target, error)),
            }
        } else {
            None
        };
        #[cfg(unix)]
        if let Some(permissions) = &permissions {
            // The empty temp may have default permissions, but its contents must
            // never be more accessible than the file being replaced.
            file.set_permissions(permissions.clone())
                .map_err(|e| StorageError::io(&temp, e))?;
        }
        copy_stream(reader, &mut file).map_err(|e| StorageError::io(&temp, e))?;
        #[cfg(unix)]
        if let Some(permissions) = permissions {
            // Writing can clear special mode bits; restore them before syncing.
            file.set_permissions(permissions)
                .map_err(|e| StorageError::io(&temp, e))?;
        }
        pre_sync(guard).map_err(|e| StorageError::io(&temp, e))?;
        sys::sync_file(&file).map_err(|e| StorageError::io(&temp, e))?;
        #[cfg(windows)]
        let _ = copy_mode;
        Ok(())
    })();
    // Windows cannot rename or delete a handle opened without share-delete.
    drop(file);
    if let Err(error) = prepared {
        return Err(clean_file(guard, error, &temp));
    }
    Ok(temp)
}

/// Copy exact bytes with fixed memory, including for explicitly named streams.
pub(crate) fn copy_stream(reader: &mut impl Read, writer: &mut impl Write) -> io::Result<()> {
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let length = match reader.read(&mut buffer) {
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            result => result?,
        };
        if length == 0 {
            return Ok(());
        }
        writer.write_all(&buffer[..length])?;
    }
}

// All Windows publication calls share the same monotonic two-second policy.
// Replacement supplies validation as part of each attempt; exclusive creation
// has no edit baseline to revalidate.
fn publication_attempts(mut attempt: impl FnMut() -> io::Result<()>) -> io::Result<()> {
    #[cfg(windows)]
    {
        let start = std::time::Instant::now();
        let mut delay = std::time::Duration::from_millis(1);
        loop {
            match attempt() {
                Err(error) if sys::retryable_publication(&error) => {
                    if !super::lock::backoff(start, std::time::Duration::from_secs(2), &mut delay) {
                        return Err(error);
                    }
                }
                result => return result,
            }
        }
    }
    #[cfg(unix)]
    attempt()
}

fn exclusive_attempts(source: &Path, target: &Path) -> io::Result<()> {
    publication_attempts(|| match sys::rename_exclusive(source, target) {
        // Windows reports an existing directory target as access denied; an
        // existing target is a collision, never transient, so do not retry.
        Err(error)
            if sys::exclusive_access_denied(&error) && fs::symlink_metadata(target).is_ok() =>
        {
            Err(io::Error::new(io::ErrorKind::AlreadyExists, error))
        }
        result => result,
    })
}

fn replace_attempts(
    guard: &WorkspaceGuard<'_>,
    temp: &Path,
    target: &Path,
    validate: &mut impl FnMut() -> Result<(), Conflict>,
) -> Result<(), StorageError> {
    let mut validation_error = None;
    #[cfg(feature = "storage-test-hooks")]
    let mut attempt = 0;
    let result = publication_attempts(|| {
        if let Err(error) = validate()
            .map_err(|e| StorageError::new(ErrorKind::Conflict(e)))
            .and_then(|()| guard.check_path(target))
        {
            validation_error = Some(error);
            return Err(io::Error::other("publication validation failed"));
        }
        #[cfg(feature = "storage-test-hooks")]
        {
            attempt += 1;
            if guard.test_hooks.fail_publish_permission_denied {
                return Err(io::Error::from(io::ErrorKind::PermissionDenied));
            }
            if guard.test_hooks.force_cross_filesystem {
                return Err(io::Error::from_raw_os_error(cross_device_code()));
            }
        }
        let result = sys::replace(temp, target);
        if result.is_err() {
            super::crash::barrier("atomic_replace.failed");
        }
        #[cfg(feature = "storage-test-hooks")]
        if let (Err(_), Some(hook)) = (&result, guard.test_hooks.replace_after_failure) {
            hook(target, attempt);
        }
        result
    });
    if let Some(error) = validation_error {
        return Err(error);
    }
    result.map_err(|error| publication_error("replace", temp, target, error))
}

fn publication_error(
    op: &'static str,
    source: &Path,
    target: &Path,
    error: io::Error,
) -> StorageError {
    if error.kind() == io::ErrorKind::AlreadyExists
        || (op != "replace"
            && sys::exclusive_access_denied(&error)
            && fs::symlink_metadata(target).is_ok_and(|metadata| metadata.is_dir()))
    {
        StorageError::new(ErrorKind::Exists {
            path: target.into(),
        })
    } else if sys::cross_device(&error) {
        StorageError::new(ErrorKind::CrossFilesystem {
            source: source.into(),
            target: target.into(),
        })
    } else if sys::flag_unsupported(&error) {
        StorageError::new(ErrorKind::Unsupported {
            op,
            filesystem: sys::filesystem(source.parent().expect("source parent")),
        })
    } else {
        StorageError::io(target, error)
    }
}

fn clean_file(guard: &WorkspaceGuard<'_>, mut error: StorageError, temp: &Path) -> StorageError {
    error.cleanup = unlink_temp(guard, temp);
    error
}

fn unlink_temp(guard: &WorkspaceGuard<'_>, temp: &Path) -> Cleanup {
    #[cfg(feature = "storage-test-hooks")]
    if guard.test_hooks.fail_unlink {
        return Cleanup::Pending(temp.into());
    }
    let _ = guard;
    match fs::remove_file(temp) {
        Ok(()) => Cleanup::Done,
        Err(error) if error.kind() == io::ErrorKind::NotFound => Cleanup::Done,
        Err(_) => Cleanup::Pending(temp.into()),
    }
}

fn pre_sync(guard: &WorkspaceGuard<'_>) -> io::Result<()> {
    #[cfg(feature = "storage-test-hooks")]
    if guard.test_hooks.fail_pre_commit_sync {
        return Err(io::Error::other("injected pre-commit sync failure"));
    }
    let _ = guard;
    Ok(())
}

fn finish(
    guard: &WorkspaceGuard<'_>,
    target: &Path,
    source_parent: &Path,
    cleanup: Cleanup,
) -> Published {
    let mut paths = vec![
        target.parent().expect("destination parent").to_path_buf(),
        source_parent.to_path_buf(),
    ];
    guard.created.borrow_mut().retain(|created| {
        !matches!(fs::symlink_metadata(created), Err(error) if error.kind() == io::ErrorKind::NotFound)
    });
    let created: Vec<_> = guard
        .created
        .borrow()
        .iter()
        .filter(|created| target.starts_with(created) || source_parent.starts_with(created))
        .cloned()
        .collect();
    for created in &created {
        paths.push(created.clone());
        if let Some(parent) = created.parent() {
            paths.push(parent.into());
        }
    }
    // Every descendant precedes its ancestors, with duplicate syncs eliminated.
    paths.sort_by(|a, b| {
        b.components()
            .count()
            .cmp(&a.components().count())
            .then_with(|| a.cmp(b))
    });
    paths.dedup();
    let mut failure = None;
    #[cfg(feature = "storage-test-hooks")]
    if guard.test_hooks.post_commit_sync_failure {
        failure = Some(io::Error::other("injected post-commit sync failure"));
    }
    let mut synced = Vec::new();
    for path in paths {
        let result = guard
            .check_path(&path)
            .map_err(io::Error::other)
            .and_then(|_| sys::sync_dir(&path));
        match result {
            Ok(()) => synced.push(path),
            Err(error) => {
                failure.get_or_insert(error);
            }
        }
    }
    guard.created.borrow_mut().retain(|path| {
        !created.contains(path)
            || !synced.contains(path)
            || path
                .parent()
                .is_some_and(|parent| !synced.iter().any(|path| path == parent))
    });
    Published {
        committed: true,
        durability: failure.map_or(Durability::Ok, Durability::Uncertain),
        cleanup,
    }
}

#[cfg(feature = "storage-test-hooks")]
fn unsupported_code() -> i32 {
    #[cfg(unix)]
    {
        libc::EOPNOTSUPP
    }
    #[cfg(windows)]
    {
        windows_sys::Win32::Foundation::ERROR_NOT_SUPPORTED as i32
    }
}

#[cfg(feature = "storage-test-hooks")]
fn cross_device_code() -> i32 {
    #[cfg(unix)]
    {
        libc::EXDEV
    }
    #[cfg(windows)]
    {
        windows_sys::Win32::Foundation::ERROR_NOT_SAME_DEVICE as i32
    }
}

#[cfg(target_os = "linux")]
fn filesystem_refuses_noreplace(error: &io::Error) -> bool {
    matches!(
        error.raw_os_error(),
        Some(libc::EINVAL | libc::EOPNOTSUPP | libc::ENOSYS)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(target_os = "linux")]
    #[test]
    fn enosys_permits_the_file_link_fallback() {
        for code in [libc::ENOSYS, libc::EINVAL, libc::EOPNOTSUPP] {
            assert!(filesystem_refuses_noreplace(&io::Error::from_raw_os_error(
                code
            )));
        }
        assert!(!filesystem_refuses_noreplace(
            &io::Error::from_raw_os_error(libc::EACCES)
        ));
    }

    #[cfg(unix)]
    #[test]
    fn permission_denied_is_not_an_exclusive_name_collision() {
        let error = publication_error(
            "publish file",
            Path::new("temp"),
            Path::new("."),
            io::Error::from_raw_os_error(libc::EACCES),
        );
        assert!(matches!(error.kind, ErrorKind::Io { .. }));
    }

    #[cfg(windows)]
    #[test]
    fn publication_retry_policy_handles_both_transient_codes() {
        use windows_sys::Win32::Foundation::{ERROR_ACCESS_DENIED, ERROR_SHARING_VIOLATION};
        for code in [ERROR_ACCESS_DENIED, ERROR_SHARING_VIOLATION] {
            let mut attempts = 0;
            publication_attempts(|| {
                attempts += 1;
                if attempts == 1 {
                    Err(io::Error::from_raw_os_error(code as i32))
                } else {
                    Ok(())
                }
            })
            .unwrap();
            assert_eq!(attempts, 2);
        }
        let mut attempts = 0;
        let error = publication_attempts(|| {
            attempts += 1;
            Err(io::Error::new(io::ErrorKind::AlreadyExists, "collision"))
        })
        .unwrap_err();
        assert_eq!(attempts, 1);
        assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
    }
}
