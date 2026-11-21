//! Workspace housekeeping. Plans and printed suggestions are read-only.
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use crate::error::Error;
use crate::exit::ExitCode;
use crate::storage::{self, WorkspaceGuard, WorkspaceLock, prune};

pub(crate) const IGNORE_PATTERNS: &str =
    ".cpforge/runs/\n.cpforge/cache/\n.cpforge/staging/\n# optional: .cpforge/facts/\n";

pub(crate) fn root() -> Result<PathBuf, Error> {
    let cwd = std::env::current_dir().map_err(|e| Error::Workspace(e.to_string()))?;
    crate::workspace::find_root(&cwd)
}

fn print(text: &str) -> Result<(), Error> {
    let mut stdout = io::stdout().lock();
    stdout
        .write_all(text.as_bytes())
        .and_then(|()| stdout.flush())
        .map_err(|e| Error::Workspace(format!("cannot write output: {e}")))
}

pub(crate) fn gitignore(write: bool) -> Result<ExitCode, Error> {
    let root = root()?;
    if write {
        let lock = WorkspaceLock::new(&root)?;
        let guard = lock.acquire("gitignore")?;
        let published = write_gitignore(&guard)?;
        let mut report = crate::workspace::Report::default();
        crate::workspace::publication_warnings(&mut report, &root.join(".gitignore"), &published);
        for warning in report.warnings {
            eprintln!("cpf: warning: {warning}");
        }
    } else {
        print(IGNORE_PATTERNS)?;
    }
    Ok(ExitCode::Success)
}

pub(crate) fn write_gitignore(guard: &WorkspaceGuard<'_>) -> Result<storage::Published, Error> {
    let path = guard.check_path(".gitignore")?;
    match fs::symlink_metadata(&path) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            let block = ignore_block(&[]).expect("empty file has no markers");
            storage::publish_file_noclobber(guard, path, &block).map_err(Error::from)
        }
        Err(error) => Err(Error::Workspace(error.to_string())),
        Ok(_) => storage::edit_text(guard, path, ignore_block)
            .map_err(|error| Error::Workspace(error.to_string())),
    }
}

fn ignore_block(bytes: &[u8]) -> Result<Vec<u8>, &'static str> {
    let mut starts = Vec::new();
    let mut ends = Vec::new();
    let mut offset = 0;
    for line in bytes.split_inclusive(|b| *b == b'\n') {
        let key = line.strip_suffix(b"\n").unwrap_or(line);
        let key = key.strip_suffix(b"\r").unwrap_or(key);
        if key == b"# >>> cpforge" {
            starts.push((offset, offset + line.len()));
        }
        if key == b"# <<< cpforge" {
            ends.push((offset, offset + line.len()));
        }
        offset += line.len();
    }
    let mut result = Vec::new();
    match (starts.as_slice(), ends.as_slice()) {
        ([], []) => {
            result.extend_from_slice(bytes);
            if !bytes.is_empty() && !bytes.ends_with(b"\n") {
                result.push(b'\n');
            }
            result.extend_from_slice(b"# >>> cpforge\n");
            result.extend_from_slice(IGNORE_PATTERNS.as_bytes());
            result.extend_from_slice(b"# <<< cpforge\n");
        }
        ([(start, after_start)], [(end, _)]) if after_start <= end => {
            result.extend_from_slice(&bytes[..*after_start]);
            result.extend_from_slice(IGNORE_PATTERNS.as_bytes());
            result.extend_from_slice(&bytes[*end..]);
            debug_assert!(start < end);
        }
        _ => return Err("invalid cpforge .gitignore markers"),
    }
    Ok(result)
}

/// `cpf clean [--apply]`: storage §6.3 pruning with a zero retention budget,
/// plus `.cpforge/cache/`. Both modes take the workspace lock (and so perform
/// the universal leftover recovery); the plan selects and renames nothing.
pub(crate) fn clean(apply: bool) -> Result<ExitCode, Error> {
    use std::fmt::Write as _;
    let root = root()?;
    let lock = WorkspaceLock::new(&root)?;
    let guard = lock.acquire("clean")?;
    if let Some(warning) = guard.recovery_warning() {
        for (path, error) in warning.paths.iter().zip(&warning.errors) {
            eprintln!(
                "cpf: warning: leftover recovery incomplete at {}: {error}",
                path.display()
            );
        }
    }
    let runs = guard.check_path(Path::new(".cpforge").join("runs"))?;
    match fs::symlink_metadata(&runs) {
        Ok(metadata) if storage::is_link(&metadata) || !metadata.is_dir() => {
            return Err(Error::Workspace(format!(
                "{} is not a directory",
                runs.display()
            )));
        }
        Err(error) if error.kind() != io::ErrorKind::NotFound => {
            return Err(Error::Workspace(format!("{}: {error}", runs.display())));
        }
        _ => {}
    }
    let cache = guard.check_path(Path::new(".cpforge").join("cache"))?;
    let cache_exists = match fs::symlink_metadata(&cache) {
        Ok(_) => true,
        Err(error) if error.kind() == io::ErrorKind::NotFound => false,
        Err(error) => return Err(Error::Workspace(format!("{}: {error}", cache.display()))),
    };
    let leftovers = reclaim_leftovers(&runs);
    let mut partial = false;
    let mut cache_reclaim = None;
    let selection = if apply {
        let selection = prune::select_and_rename(&guard, prune::PrunePolicy::zero());
        if cache_exists {
            match stage_cache(&guard, &cache) {
                Ok(target) => cache_reclaim = Some(target),
                Err(error) => {
                    eprintln!("cpf: warning: .cpforge/cache/ not removed: {error}");
                    partial = true;
                }
            }
        }
        selection
    } else {
        prune::plan(&guard, prune::PrunePolicy::zero())
    };
    // Step 3 runs outside the workspace lock (§6.3).
    drop(guard);
    let report = &selection.report;
    for error in &report.errors {
        eprintln!("cpf: warning: {error}");
    }
    for error in &report.unreadable_metadata {
        eprintln!("cpf: warning: kept (unreadable record): {error}");
    }
    for (path, error) in &report.failed_probes {
        eprintln!(
            "cpf: warning: kept {}: lease cannot be probed: {error}",
            path.display()
        );
    }
    for failure in &report.failed_renames {
        eprintln!(
            "cpf: warning: .cpforge/runs/{} not removed: {}",
            failure.job, failure.error
        );
    }
    for (path, error) in &report.usage.errors {
        eprintln!("cpf: warning: {}: {error}", path.display());
    }
    for skipped in &report.usage.skipped {
        eprintln!(
            "cpf: warning: skipped {}: {:?}",
            skipped.path.display(),
            skipped.reason
        );
    }
    for path in &report.non_job_entries {
        eprintln!("cpf: warning: unexpected entry kept: {}", path.display());
    }
    partial |= !report.failed_renames.is_empty();
    let mut output = String::new();
    if apply {
        let reclaimed = prune::reclaim(&selection.runs_dir);
        let removed = |path: &Path| reclaimed.reclaimed.iter().any(|done| done == path);
        for id in &selection.renamed {
            if removed(&selection.runs_dir.join(id.reclaim_name())) {
                let _ = writeln!(output, "removed .cpforge/runs/{id}/");
            }
        }
        if cache_reclaim.as_deref().is_some_and(removed) {
            output.push_str("removed .cpforge/cache/\n");
        }
        for name in &leftovers {
            if removed(&selection.runs_dir.join(name)) {
                let _ = writeln!(output, "removed .cpforge/runs/{name}");
            }
        }
        partial |= !reclaimed.errors.is_empty()
            || !reclaimed.walk.errors.is_empty()
            || !reclaimed.walk.skipped.is_empty();
        for error in reclaimed.errors {
            eprintln!("cpf: warning: {error}");
        }
        for (path, error) in reclaimed.walk.errors {
            eprintln!("cpf: warning: {}: {error}", path.display());
        }
        for skipped in reclaimed.walk.skipped {
            eprintln!(
                "cpf: warning: skipped {}: {:?}",
                skipped.path.display(),
                skipped.reason
            );
        }
    } else {
        for id in &selection.candidates {
            let _ = writeln!(output, "would remove .cpforge/runs/{id}/");
        }
        if cache_exists {
            output.push_str("would remove .cpforge/cache/\n");
        }
        for name in &leftovers {
            let _ = writeln!(output, "would remove .cpforge/runs/{name}");
        }
    }
    for id in &report.skipped_active {
        let _ = writeln!(output, "kept .cpforge/runs/{id}/: active");
    }
    for protected in &report.protected {
        let _ = writeln!(
            output,
            "kept .cpforge/runs/{}/: {} has newer schema {}",
            protected.job,
            protected.path.display(),
            protected.schema
        );
    }
    if output.is_empty() {
        output.push_str("nothing to clean\n");
    }
    print(&output)?;
    Ok(if partial {
        ExitCode::Partial
    } else {
        ExitCode::Success
    })
}

/// The cache takes the same rename-then-delete path as job directories: a
/// reserved reclaim name under `runs/`, so a later prune retries an
/// interrupted deletion.
fn stage_cache(guard: &WorkspaceGuard<'_>, cache: &Path) -> Result<PathBuf, Error> {
    let runs = guard.create_dir_all(Path::new(".cpforge").join("runs"))?;
    let mut random = [0; 16];
    getrandom::fill(&mut random).map_err(|e| Error::Internal(e.to_string()))?;
    let target = runs.join(format!(
        ".reclaim-cache-{:032x}",
        u128::from_le_bytes(random)
    ));
    prune::rename(guard, cache, &target)?;
    Ok(target)
}

/// Names of earlier interrupted reclamations (`runs/.reclaim-*`), sorted.
fn reclaim_leftovers(runs: &Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(runs)
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .filter(|name| name.starts_with(".reclaim-"))
        .collect();
    names.sort();
    names
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn block_replacement_preserves_arbitrary_outside_bytes() {
        let input = b"\xffuser\r\n# >>> cpforge\nold\n# <<< cpforge\n\xfe";
        let output = ignore_block(input).unwrap();
        assert!(output.starts_with(b"\xffuser\r\n# >>> cpforge\n"));
        assert!(output.ends_with(b"# <<< cpforge\n\xfe"));
    }

    #[cfg(feature = "storage-test-hooks")]
    fn directory() -> PathBuf {
        let mut random = [0; 16];
        getrandom::fill(&mut random).unwrap();
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("target")
            .join("gitignore-fixtures")
            .join(format!("{:032x}", u128::from_le_bytes(random)));
        fs::create_dir_all(&path).unwrap();
        path
    }

    #[cfg(feature = "storage-test-hooks")]
    #[test]
    fn concurrent_edit_of_user_text_refuses_and_preserves_editor_bytes() {
        fn editor(path: &Path) {
            fs::write(
                path,
                b"concurrent user edit\n# >>> cpforge\nold\n# <<< cpforge\n",
            )
            .unwrap();
        }
        let root = directory();
        let lock = WorkspaceLock::new(&root).unwrap();
        let guard = lock
            .acquire_with_options(
                "concurrent gitignore",
                storage::LockOptions {
                    test_hooks: storage::TestHooks {
                        edit_after_read: Some(editor),
                        ..storage::TestHooks::default()
                    },
                    ..storage::LockOptions::default()
                },
            )
            .unwrap();
        let path = root.join(".gitignore");
        fs::write(&path, b"user text\n# >>> cpforge\nold\n# <<< cpforge\n").unwrap();
        let error = write_gitignore(&guard).unwrap_err();
        assert_eq!(error.exit_code(), ExitCode::Workspace);
        assert!(error.to_string().contains("concurrent edit"), "{error}");
        assert_eq!(
            fs::read(&path).unwrap(),
            b"concurrent user edit\n# >>> cpforge\nold\n# <<< cpforge\n"
        );
        drop(guard);
        drop(lock);
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(feature = "storage-test-hooks")]
    #[test]
    fn absent_gitignore_publication_does_not_clobber_a_racing_create() {
        fn editor(path: &Path) {
            fs::write(path, b"created by editor\n").unwrap();
        }
        let root = directory();
        let lock = WorkspaceLock::new(&root).unwrap();
        let guard = lock
            .acquire_with_options(
                "gitignore create race",
                storage::LockOptions {
                    test_hooks: storage::TestHooks {
                        before_file_publish: Some(editor),
                        ..storage::TestHooks::default()
                    },
                    ..storage::LockOptions::default()
                },
            )
            .unwrap();
        let error = write_gitignore(&guard).unwrap_err();
        assert_eq!(error.exit_code(), ExitCode::Workspace);
        assert_eq!(
            fs::read(root.join(".gitignore")).unwrap(),
            b"created by editor\n"
        );
        drop(guard);
        drop(lock);
        fs::remove_dir_all(root).unwrap();
    }
}
