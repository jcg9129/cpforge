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
//! `fact.before_replace`, `fact.after_replace` (fact commit boundaries);
//! `fsck.fact_removed`, `fsck.sequence_replaced` (record repair commits);
//! `gitignore.locked` (after recovery), `gitignore.published` (complete text
//! replacement or no-clobber creation);
//! `job.record_published` (after no-clobber publication of job.toml);
//! `admission.job_seq_replaced` (job-seq advanced, no job directory yet),
//! `admission.directory_created`, `admission.job_record_published`,
//! `admission.lease_created` (lease exists but has no lock),
//! `admission.lease_locked`, `admission.lock_released` (before reclaim);
//! `prune.before_rename`, `prune.after_rename` (each candidate's commit),
//! `prune.before_reclaim` (step 3 entry, after releasing the workspace guard);
//! `add.locked` (after recovery), `add.staged` (complete unpublished tree),
//! `add.published` (complete problem directory), `add.contest_created`
//! (schema-only contest file published), `add.member_added` (membership edit
//! completed, including an unchanged existing membership).
//! `test_add.locked` (after recovery), `test_add.input_published`,
//! `test_add.answer_published`, `test_add.registered` (catalog insertion).
//! Organisation clock: `CPF_STORAGE_ORG_NOW=YYYY-MM-DDTHH:MM:SS.mmmZ`
//! overrides the single invocation clock sample for tag/untag/mark/add.
//! Search bounds: `CPF_STORAGE_SEARCH_ENTRIES` and `CPF_STORAGE_SEARCH_BYTES`
//! lower E and B. `CPF_STORAGE_SEARCH_IO=<path>` records action number, call,
//! source path and bytes read as tab-separated rows. Only test builds inspect
//! these variables or open the recorder. Barriers can leave the read worker
//! stalled until process exit to exercise cancellation independently of I/O.
//! Organisation barriers: `org.after_resolve`, `org.before_validation`,
//! `notes.before_publish`, `org.read_partial` (after the first half of a
//! record/notes read), `search.before_action#k` (before counted action k),
//! `search.read#k` (stall read k), `search.stdout` (stall delivery); crash points:
//! `org.edit_validated`,
//! `org.edit_committed` (rename complete, directory not yet synced),
//! `notes.before_publish`, `notes.published`.
//! Selecting `file_noclobber.linked` also forces the Linux link fallback in
//! test builds, so real command subprocesses can exercise that boundary.
//!
//! Pause barriers: set `CPF_STORAGE_PAUSE_AT=<name>` to print
//! `CPF-PAUSED <name>` to stdout and block until one byte arrives on stdin.
//! Barriers: `inputs.source_copied` (after the first captured source copy),
//! `fact.cutoff`, `fact.after_cutoff`, `fact.after_replace`;
//! `atomic_replace.failed` (after a native failed replace, before retry);
//! `command.between_iterations` (after the first command job execution);
//! `config.load`, `selection.resolve` (planning, before admission);
//! `supervisor.before_cancel_kill` (live root before orderly cancellation kill);
//! `supervisor.after_registration` (registered root before creation replies);
//! `job.after_delivery` (after the final summary, while signals remain installed),
//! `stderr.write` (before the first progress-writer write), `stderr.lock`
//! (inside the stderr queue lock, at every offer while selected),
//! `manifest.publish` (before publishing inputs/capture.json).
//! Multiple names may be selected with commas. With `CPF_STORAGE_BARRIER_DIR`,
//! each selected barrier awaits a file named `<name>.release` in that directory
//! instead of stdin (hard deadline: 20 s). This keeps independent threads and
//! raw console stdin from consuming one another's release bytes.
//! Set `CPF_STORAGE_PANIC_AT=hook.execute` to panic in the executor of an
//! admitted hook, before launching its child. Only test builds inspect it.
//! Fact warning injection: `CPF_STORAGE_FACT_TEST_HOOK=post_commit_sync_failure`
//! uses the guard's sync failure hook only during fact publication;
//! `cleanup_pending` injects that publication result flag for the report adapter
//! (successful atomic replacement itself leaves no temp). Set
//! `CPF_STORAGE_FACT_RECOVERY_REMOVE_FAILURE=<path>` to fail recovery of that
//! exact leftover only on the fact's lock acquisition.
//! Golden fact vectors: `CPF_STORAGE_FIXED_JOB_ID=<valid id>` overrides job-id
//! generation and `CPF_STORAGE_FIXED_FACT_TIME_MS=<Unix milliseconds>` supplies
//! the fact publication clock. Each fixed id still requires an exclusive directory.
//! `CPF_STORAGE_SIGNAL_READY=<path>` publishes a first-notification acknowledgement
//! from the control thread, for deterministic force-exit debounce tests.
//! `CPF_STORAGE_RUN_WAITING=<path>` publishes the pid when the executor starts
//! waiting for a root: fixture identity files alone do not establish this.
//! `CPF_STORAGE_STDERR_QUEUED=<path>` and `CPF_STORAGE_CONSOLE_DRAIN=<path>`
//! receive the wall-clock nanoseconds of the final stderr batch enqueue (t_q)
//! and of the raw console drain milestone, for subprocess timing fixtures.
//! `CPF_STORAGE_PERSISTENCE_STATS=<path>` records queue credit wait, service
//! cycle samples and W-to-finished timing after the engine returns.
//! `persist.write` pauses the persistence worker before a write.
//! `judge.finish` pauses a judge run's evidence/scratch-removal work (on its
//! post-run worker, or on the creation thread when no credit was free).
//! File-I/O workers share this barrier protocol at `file_io.before_listing`,
//! `file_io.listing`, `file_io.listing.entry`, `file_io.type_check`,
//! `file_io.open`, `file_io.read`, `file_io.write`, `file_io.staging.write`,
//! `file_io.after_listing` and `file_io.extracted` (faults and stub controls
//! are documented in supervisor/file_io_hooks.rs). `judge.before_output_copy`
//! pauses after the solution's capture length is frozen; with
//! `CPF_FILE_IO_GROW_STDOUT=<suffix>` that source (the stdout capture or the
//! extracted `out/output`) grows before the checker copy.
//! `CPF_STORAGE_CREATION_SOCKET=<Unix socket path>` holds OS creation in
//! pre_exec: the child writes its native-endian i32 pid, then awaits one byte
//! from the harness; EOF instead fails pre_exec, so the child exits without exec.
//! The harness keeps it held across force exit, then closes its end.

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

/// Feature-gated subprocess barrier after the first source has been copied.
/// Test drivers exchange one byte over their inherited stdin/stdout pipes;
/// production builds do not inspect this setting or perform pipe I/O.
#[inline]
pub(crate) fn barrier(name: &str) {
    #[cfg(feature = "storage-test-hooks")]
    {
        use std::io::{Read, Write};
        if std::env::var("CPF_STORAGE_PAUSE_AT")
            .is_ok_and(|value| value.split(',').any(|selected| selected == name))
        {
            println!("CPF-PAUSED {name}");
            std::io::stdout().flush().expect("flush storage barrier");
            if let Some(directory) = std::env::var_os("CPF_STORAGE_BARRIER_DIR") {
                let release = std::path::PathBuf::from(directory).join(format!("{name}.release"));
                let until = std::time::Instant::now() + std::time::Duration::from_secs(20);
                while !release.exists() {
                    assert!(std::time::Instant::now() < until, "release barrier {name}");
                    std::thread::sleep(std::time::Duration::from_millis(1));
                }
            } else {
                std::io::stdin()
                    .read_exact(&mut [0_u8; 1])
                    .expect("release storage barrier");
            }
        }
    }
    #[cfg(not(feature = "storage-test-hooks"))]
    let _ = name;
}

#[cfg(all(
    feature = "storage-test-hooks",
    any(target_os = "linux", target_os = "macos")
))]
pub(crate) fn run_waiting(pid: u32) {
    if let Some(path) = std::env::var_os("CPF_STORAGE_RUN_WAITING") {
        std::fs::write(path, pid.to_string()).expect("publish root wait readiness");
    }
}

/// Acknowledges a handled first signal on the control thread, never in the OS
/// handler. Tests start the debounce interval only after observing this file.
#[inline]
pub(crate) fn signal_observed() {
    #[cfg(feature = "storage-test-hooks")]
    if let Some(path) = std::env::var_os("CPF_STORAGE_SIGNAL_READY") {
        std::fs::write(path, b"signal observed\n").expect("publish signal readiness");
    }
}

/// Inject an unwind at a named job boundary, rather than aborting the process.
#[cfg(all(
    feature = "storage-test-hooks",
    any(target_os = "linux", target_os = "macos")
))]
pub(crate) fn panic_point(name: &'static str) {
    if std::env::var("CPF_STORAGE_PANIC_AT").is_ok_and(|value| value == name) {
        panic!("CPF-PANIC-AT {name}");
    }
}

/// Debug milestone recorded just after the final stderr batch enqueue (t_q).
/// It is outside the queue lock and never uses the human stderr sink.
#[cfg(feature = "storage-test-hooks")]
pub(crate) fn stderr_final_queued(queued_at: std::time::Instant) {
    timing_milestone("CPF_STORAGE_STDERR_QUEUED", queued_at);
}

/// Debug origin of the raw console's independent forwarding-flush clock.
#[cfg(all(
    feature = "storage-test-hooks",
    any(target_os = "linux", target_os = "macos")
))]
pub(crate) fn console_drain_started(at: std::time::Instant) {
    timing_milestone("CPF_STORAGE_CONSOLE_DRAIN", at);
}

#[cfg(feature = "storage-test-hooks")]
fn timing_milestone(variable: &str, at: std::time::Instant) {
    if let Some(path) = std::env::var_os(variable) {
        // Map the monotonic origin to wall time only for the subprocess
        // observer. Policy continues to use the original monotonic deadline.
        let wall = std::time::SystemTime::now() - at.elapsed();
        let nanos = wall
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::fs::write(path, nanos.to_string()).expect("publish stderr milestone");
    }
}
