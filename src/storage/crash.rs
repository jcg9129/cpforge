//! Process-crash points, enabled only by `storage-test-hooks`.
//!
//! Set `CPF_STORAGE_CRASH_AT=<operation>.<phase>[#<n>]` to abort at the
//! one-based occurrence (default 1), counted independently for each point in
//! this process. The marker is flushed to stderr before aborting.
//!
//! Points: `atomic_replace.temp_synced`, `atomic_replace.renamed`,
//! `atomic_replace.directory_synced`; `file_noclobber.before_publish`,
//! `file_noclobber.after_publish`, `file_noclobber.directory_synced`,
//! `file_noclobber.linked` (Linux fallback,
//! before unlink); `dir_noclobber.staging_synced`, `dir_noclobber.renamed`,
//! `dir_noclobber.directories_synced`; `edit_toml.validated` (immediately
//! before each replacement attempt), `edit_toml.published`;
//! `lock.acquired` (before recovery), `lock.recovered`;
//! `init.locked` (after lock acquisition and recovery),
//! `init.job_seq_published` (after publishing a new initial job-seq),
//! `init.config_published` (after publishing cpforge.toml);
//! `job.record_published` (after no-clobber publication of job.toml);
//! `add.locked` (after recovery), `add.staged` (complete unpublished tree),
//! `add.published` (complete problem directory), `add.contest_created`
//! (schema-only contest file published), `add.member_added` (membership edit
//! completed, including an unchanged existing membership).
//! `test_add.locked` (after recovery), `test_add.input_published`,
//! `test_add.answer_published`, `test_add.registered` (catalog insertion).
//! Selecting `file_noclobber.linked` also forces the Linux link fallback in
//! test builds, so real command subprocesses can exercise that boundary.

#[cfg(all(feature = "storage-test-hooks", target_os = "linux"))]
pub(super) fn force_link_fallback() -> bool {
    std::env::var("CPF_STORAGE_CRASH_AT")
        .is_ok_and(|selected| selected.split('#').next() == Some("file_noclobber.linked"))
}

#[inline]
pub(crate) fn point(name: &'static str) {
    #[cfg(feature = "storage-test-hooks")]
    {
        use std::collections::HashMap;
        use std::io::Write;
        use std::sync::{Mutex, OnceLock};

        static COUNTS: OnceLock<Mutex<HashMap<&'static str, u64>>> = OnceLock::new();
        let Ok(setting) = std::env::var("CPF_STORAGE_CRASH_AT") else {
            return;
        };
        let (selected, occurrence) = match setting.rsplit_once('#') {
            Some((selected, count)) => match count.parse::<u64>() {
                Ok(count) if count > 0 => (selected, count),
                _ => return,
            },
            None => (setting.as_str(), 1),
        };
        if selected != name {
            return;
        }
        let mut counts = COUNTS
            .get_or_init(Mutex::default)
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        let count = counts.entry(name).or_default();
        *count += 1;
        if *count == occurrence {
            let mut stderr = std::io::stderr().lock();
            let _ = writeln!(stderr, "CPF-CRASH-AT {name}");
            let _ = stderr.flush();
            std::process::abort();
        }
    }
    #[cfg(not(feature = "storage-test-hooks"))]
    let _ = name;
}
