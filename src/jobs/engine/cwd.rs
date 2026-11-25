//! Bounded, diagnostic-only post-run working-directory retention.

use crate::report::RunNote;
use crate::storage::job::JobOwner;
use crate::supervisor::CancelToken;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

/// Policy deadline for the whole step, from its start (never scaled).
#[cfg(any(target_os = "linux", target_os = "macos"))]
const DEADLINE: std::time::Duration = std::time::Duration::from_secs(5);

/// Marks the job's single outstanding walker; cleared when the walker's work
/// (or the refused attempt) is dropped, on whichever thread that happens.
#[cfg(any(target_os = "linux", target_os = "macos"))]
struct Walking(Arc<AtomicBool>);

#[cfg(any(target_os = "linux", target_os = "macos"))]
impl Drop for Walking {
    fn drop(&mut self) {
        self.0.store(false, std::sync::atomic::Ordering::Release);
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn absent(error: &crate::storage::StorageError) -> bool {
    use crate::storage::ErrorKind;
    matches!(&error.kind, ErrorKind::Io { source, .. } if source.kind() == std::io::ErrorKind::NotFound)
}

pub(super) async fn finish(
    owner: JobOwner,
    path: PathBuf,
    limit: u64,
    cancel: CancelToken,
    walking: &Arc<AtomicBool>,
    #[cfg(feature = "storage-test-hooks")] observation: Option<super::Observation>,
    #[cfg(feature = "storage-test-hooks")] execution: u32,
) -> Result<Option<u64>, RunNote> {
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    {
        use std::sync::atomic::Ordering;

        // One policy deadline includes worker admission, measurement and clear.
        let deadline = tokio::time::Instant::now() + DEADLINE;
        // An earlier walker of this job is still blocked past its own deadline:
        // do not stack another credited thread on the same stalled filesystem.
        if walking
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return Err(RunNote::CwdUnmeasuredTimeout);
        }
        let walking = Walking(walking.clone());
        let worker_cancel = cancel.clone();
        let worker_deadline = deadline.into_std();
        crate::supervisor::post_run(&cancel, deadline, move || {
            let _walking = walking;
            // Keep the lease (and its retained-byte accounting) with the actual
            // walker, including after timeout, cancellation or future drop.
            let guard = crate::storage::WorkspaceGuard::leased(&owner);
            // Checked before each walk only: a completed result is reported.
            let check = || {
                if worker_cancel.is_cancelled() {
                    Err(RunNote::CwdUnmeasuredCancelled)
                } else if std::time::Instant::now() >= worker_deadline {
                    Err(RunNote::CwdUnmeasuredTimeout)
                } else {
                    Ok(())
                }
            };
            check()?;
            #[cfg(not(feature = "storage-test-hooks"))]
            let measured = crate::storage::measure_tree(&guard, &path);
            #[cfg(feature = "storage-test-hooks")]
            let measured = crate::storage::measure_tree_observed(&guard, &path, &mut |entry| {
                if entry == path.as_path() {
                    if let Some(observer) = &observation {
                        (observer.0)(super::Point::BeforeCwdMeasure(execution));
                    }
                }
            });
            let measured = match measured {
                Ok(measured) => measured,
                // An absent cwd/ holds nothing to measure or clear.
                Err(error) if absent(&error) => return Ok(None),
                Err(_) => return Err(RunNote::CwdUnmeasuredIo),
            };
            if !measured.errors.is_empty() || !measured.skipped.is_empty() {
                return Err(RunNote::CwdUnmeasuredIo);
            }
            if measured.bytes <= limit {
                return Ok(None);
            }
            // Never start deleting once the waiter may have given up.
            check()?;
            #[cfg(not(feature = "storage-test-hooks"))]
            let cleared =
                crate::storage::remove_tree(&guard, &path).map_err(|_| RunNote::CwdUnmeasuredIo)?;
            #[cfg(feature = "storage-test-hooks")]
            let cleared = crate::storage::remove_tree_observed(&guard, &path, &mut |entry| {
                if entry == path.as_path() {
                    if let Some(observer) = &observation {
                        (observer.0)(super::Point::BeforeCwdClear(execution));
                    }
                }
            })
            .map_err(|_| RunNote::CwdUnmeasuredIo)?;
            if !cleared.errors.is_empty() || !cleared.skipped.is_empty() {
                return Err(RunNote::CwdUnmeasuredIo);
            }
            Ok(Some(measured.bytes))
        })
        .await?
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        // Production refuses execution on these platforms before admission.
        let _ = (owner, path, limit, cancel, walking);
        #[cfg(feature = "storage-test-hooks")]
        let _ = (observation, execution);
        Err(RunNote::CwdUnmeasuredIo)
    }
}
