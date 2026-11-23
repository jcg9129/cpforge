# Runtime and supervisor decisions

Status: implementation and acceptance coverage. This record states how `cpf`
runs and supervises child processes, the intended guarantees on each operating
system, and the evidence and remaining gaps for each acceptance case.
The normative rules are in the [execution contract](../contracts/execution.md) (cited as §n). The
Darwin details are in the [macOS supervisor design](../design-macos-supervisor.md).
The contract defines requirements; implementation differences and incomplete
acceptance evidence are identified below rather than treated as guarantees.

Platforms: Linux (kernel ≥ 5.4) and macOS (13+) are supported. CI validates
Linux on `ubuntu-latest` and macOS on `macos-latest`; the workflow does not
pin a macOS release or architecture. Other macOS versions and architectures
are not separate matrix entries. The Windows supervisor is **not implemented**. See [Windows](#windows-current-limitation).

## 1. Decisions

### 1.1 Runtime model

The design left open a choice between Tokio and a small thread-based job
runner. The decision is **Tokio, single-threaded per job, plus a fixed set of
dedicated OS threads for creation, sinks and persistence.**

| Element | Decision |
|---|---|
| Job executor | One Tokio **current-thread** runtime per job command, built in `job_command` and driven with `block_on` on the CLI main thread. Hook-only jobs (`cpf switch`) use their own current-thread runtime in the same way. No multi-threaded scheduler drives a job. |
| Async work | Pipe readers (async tasks on non-blocking pipe ends), exit observers (pidfd or kqueue registered with `AsyncFd`), deadline and backoff timers, cancellation, session relays and the event channel. |
| Blocking work | Run setup and post-build validation use the creation thread; sink writes and artifact publication use their own workers (table 1.2). This is not a blanket absence of synchronous I/O in async code: cleanup scans and engine comparison file reads still execute synchronously. |
| Tokio features | `process` is off. `tokio::process` and `std::process::Child::{wait, try_wait, kill}` are not used in production, and CI checks that no crate enables the feature (§3.1.3). In production, only `supervisor::sys` observes or consumes child status, by pid (apart from `std` rolling back a failed spawn). Test harnesses also use `Child` waits. It never waits on `-1` or on a group. |

Why:

- The asynchronous part of supervision is waiting on readiness: exit
  notifications, pipe data, timers and cancellation. One reactor thread
  drives the tasks; locks still arbitrate cancellation, capture state and
  signalling across worker threads.
- A single thread makes "several events ready in one wakeup" a defined case.
  The supervisor reconciles timer and exit with an explicit exit check
  (§6.3), so selection order never decides a verdict.
- Creation, sink I/O and persistence can block for an unbounded time (slow
  disks, FUSE, a stalled `rename`). On dedicated threads they cannot stall
  the reactor during those operations. Supervisor workers use explicit thread
  credits (§3.3), instead of Tokio's blocking pool. The manifest enables
  `rt-multi-thread`, but job execution uses the current-thread builder.

### 1.2 Threads

Run workers are created **before** the child exists; cleanup needs no new
thread. Worker-start failure is `NotStarted{Worker}`; OS spawn failure is
`NotStarted{CreateFailed}`. Already-started workers that stall during rollback
keep their reservations until they return (`tests/supervisor_linux.rs::failed_capture_setup_transfers_a_sink_stalled_on_close`).
The CLI atomically reserves creation and persistence's two credits before
starting either worker; failure leaves no credit reserved and refuses the job.

| Thread | Scope | Created | Does |
|---|---|---|---|
| Cleanup service (`cpf-cleanup`) | process | at `init_process`, before any job | owns transferred runs. Polls and reaps their roots with backoff, receives completions from transferred workers, publishes logical in-memory outcomes at the cutoff. It does not persist them. Uses non-consuming observation and consuming waits; Linux scans can occupy the poller until their deadline. |
| Creation (`cpf-create`) | job | at job start, holds 1 credit | FIFO queue of blocking setup: run-directory creation, recipe resolution (deferred program completion, staging), opening stdin and sink files, pipe creation, the `spawn` call, and post-build validation (§3.4). |
| Persistence (`cpf-persistence`) | job; the cleanup service if stalled when the job drops it | at job start, holds 1 credit; never a cleanup slot | temp file + rename for spec, record, `cleanup.log` and report, fed by a 16-entry queue (§6.2). A transferred worker keeps its credit, job-owner clone and metadata reservations until cleanup joins its finished thread. |
| Sink worker (`cpf-sink`) | per captured `File` stream | during setup, before spawn | blocking writes of captured bytes to the run directory (§5.1); `Discard`/memory captures have no sink thread. |
| Console forwarder (`cpf-console`), stdin forwarder (`cpf-stdin`) | per forwarded output stream, plus one stdin helper in raw `cpf run` | during setup, before spawn | 1 MiB output queue per stream, 64 KiB stdin queue; console cutoff 2 s from drain (§8.1). The stdin helper may remain blocked until process exit. |
| Transcript writers (`cpf-transcript-*`) | session | at pair admission | bounded transcript files of connected runs. |
| Control (`cpf-control`) | process | with the signal listener | turns SIGINT/SIGTERM from a self-pipe into cancellation or force exit (§8.2). Never the executor or the creation thread. |
| Output (`cpf-stdout`, `cpf-stderr`) | command | when needed | `cpf-stdout` delivers events on a separate current-thread runtime; `cpf-stderr` handles progress/summary lines. Final JSON delivery runs on the caller after supervision, and switch-hook replay writes stderr directly. |

The implementation has 4 cleanup slots (at most 4 supervisor runs admitted
concurrently; a connected pair consumes 2) and 32 thread credits, including one
held permanently by cleanup. These credits cover creation, persistence, sink,
forwarding and transcript workers. Control and output threads do not reserve
credits; at most one each of `cpf-control`, `cpf-stdout` and `cpf-stderr`
are outside the credits (§3.3). The process-wide maximum is 32 credited
threads plus these three. Unix observers need no dedicated thread.

**Creation and cancellation.** A single atomic orders commit against cancel
(`src/supervisor/creation.rs::CreationState`). Before commit, cancellation can
return `NotStarted{CancelledBeforeStart}` while setup is still blocked; no
child is then created. Resources remain with the worker until it returns.

After commit, cancellation publishes intent and returns `Transferred`
without awaiting the worker reply (`receive`, `Cancel::Committed`); a reply
already delivered is still taken, and the caller's wait kills that root.
Child existence is unknown at a `Transferred` return.
Closing the reply receiver makes the worker drop and transfer any created
root once creation returns; cleanup kills and reaps it. Connected pairs
follow the same rule. The live engine persists the cancellation snapshot
and report normally. Dropping the future uses the same ownership transfer,
with only a best-effort in-memory ticket outcome. R8 and R9 cover blocked
creation and force-exit responsiveness separately.

Run-directory creation and recipe resolution stop at step boundaries.
Post-build validation is awaited because it decides the build result.

The deadline origin is the OS spawn return (`create_end`), before observer
registration finishes. `RootTiming.setup` is recorded
and lies outside the deadline guarantee.

### 1.3 Exit observers

| OS | Primary observer | Fallback | Notes |
|---|---|---|---|
| Linux | `pidfd_open(pid, 0)` registered with `AsyncFd` | Any `pidfd_open` error: threadless polling with `waitid(P_PID, WEXITED \| WNOHANG \| WNOWAIT)`, backoff 1, 2, 4 … 10 ms. `ENOSYS`/`EPERM` add `Note::ObserverFallback`; other errnos add `SupervisorError::Register`. | Polling backoff is capped at 10 ms (scheduling can add delay); `Timing.observer` records `Polling`, not an added-delay measurement. |
| macOS | One CLOEXEC kqueue per run, `EVFILT_PROC`/`NOTE_EXIT` with `EV_ADD \| EV_RECEIPT`, registered with `AsyncFd` | Error receipt: `Register` + polling. `ESRCH` receipt: the root has already exited (exit latched). | Exit events are accepted only with a matching pid, filter and `NOTE_EXIT`. A consumed event stays latched across cancellation of the observer future. Darwin lacks `pipe2`/`kqueue1`, so a creation mutex serializes pipe/kqueue creation + `FD_CLOEXEC` with every spawn. |
| Windows | Planned: waiter thread on the process handle (§4.3) | Planned: cleanup-service polling of `WaitForSingleObject(process, 0)` | Not implemented. |

On Unix the root is spawned with `process_group(0)`, so the root's pid is the
pgid. `killpg` is allowed only while the root is known unreaped. The capability is
cleared under its per-run mutex, shared with force exit, **before** the
consuming `wait4`. Any wait
error that may lose identity protection (`ECHILD`, or anything other than
`EINTR`) clears it at once and yields status `Unknown` (§3.1.6). A `wait4`
returning 0 right after readiness is retried with backoff up to the reap
deadline. After that the still-owned root is transferred and the cleanup
service keeps polling. Wait errors revoke numeric-pid use rather than retrying
a potentially reused identity. Ordinary timeout transfer retains the root;
raw process teardown has the §8.1 exception.

### 1.4 Cleanup verification

`Confirmed` means only this: the stated algorithm, run after the group kill,
observed no live (non-zombie) member of the group other than the root. It is
not proof against `setsid`/`setpgid` escapes and says nothing about I/O
completion. Scans repeat until `Confirmed` or the 1 s cleanup deadline.
macOS uses 1, 2, 4 … 10 ms rescan backoff; the ordinary Unix wait loop retries
Linux scans after 1 ms, while transferred scans use the cleanup poller’s
backoff. Only a completed scan of a supported view can confirm. `Unverified`
does not block a pass. `Incomplete` does.

| OS | Method | Becomes `Unverified` when |
|---|---|---|
| Linux | Iterate every numeric `/proc` entry and parse `state`/`pgrp` from `/proc/<pid>/stat` into one reused 4 KiB buffer. The group id is the root's number **in this procfs instance**, from the `Pid:` field of the pidfd's `fdinfo`. No PID-namespace equality is assumed. | no pidfd (polling fallback) or `Pid: 0` → `NamespaceUnknown`. `hidepid`/unparseable mount options → `ProcFiltered`. Oversized/unparseable record → `Malformed`. Missing/unusable procfs → `ProcUnavailable`; directory/stat I/O error → `Io`. Deadline mid-scan with no completed scan → `ScanIncomplete`. |
| macOS | `proc_listpids(PROC_PGRP_ONLY, pgid)` into a buffer of up to 64 Ki pids reserved per root, then `proc_pidinfo(PROC_PIDTBSDINFO, arg = 1)` per pid. `errno` is cleared before and checked after each call. Only exact-size replies are read, and membership and liveness are rechecked from `pbi_pgid`/`pbi_status`. | full 64 Ki buffer → `Truncated`. Bad byte count or negative pid → `Malformed`. Info error or short reply → `InfoFailed`. `ENOSYS`/`ENOTSUP` → `Unsupported`. List error → `Io`. |
| Windows | Planned: `JobObjectBasicAccountingInformation.ActiveProcesses == 0`, polled and queried once more at the deadline. The completion-port message is only a wakeup. | not implemented |

The two scanners resolve the deadline in the same way. When the deadline cuts
a rescan short, the most recent completed scan that found live members gives
the `Incomplete` survivor count. A rescan that was cut short has not seen those
members leave. An earlier (or equal) reap deadline takes precedence, and the
scan object moves with the transferred root. macOS retains its entry index
and yields after at most 64 entries or a 1 ms work slice; Linux restarts a
whole directory scan on each scan attempt. Its synchronous procfs I/O, and the test
entry-delay sleep, can occupy the reactor or cleanup thread until the scan
deadline. Neither OS can pre-empt an individual kernel call. The ordinary
cleanup deadline is computed after the post-exit kill in `finish_observed`,
whereas §5.5 specifies its origin as root exit observation.

### 1.5 Deadlines and test allowances

The following are the contract bounds (§5.5, §10.1). Policy deadlines are
fixed and never scaled; only the bracketed test allowances are multiplied by
2 for hosted Linux CI and 4 for macOS and Windows CI. These allowances are assumptions about hosted
runners. Actual tests sometimes use wider bounds; those gaps are marked P
in §2, not changes to the contract.

| Path (§10.1) | Policy deadline / origin | Test allowance (Linux) |
|---|---|---|
| timer fire → kill requested | — | 50 ms |
| kill → root exit observed, normal tree | — | 250 ms |
| root exit → cleanup result | 1 s from root exit observed | +50 ms |
| cleanup result → drain start, immediate reap | — | 50 ms |
| cleanup result → drain start, consuming-wait retries | remaining time to reap deadline, 2 s from kill or root exit observed without prior kill | +50 ms |
| drain start → readers stopped, pipes closed by tree | — | 100 ms |
| drain start → readers stopped, pipe held outside containment | 500 ms from drain start | +100 ms; `NoEofBeforeDrainDeadline` |
| readers stopped → capture frozen, healthy sink | — | 100 ms |
| drain start → capture frozen, stalled sink | 5 s from drain start | +100 ms; `WriteStalled` |
| capture frozen → record finalized | — | 100 ms |
| record finalized → persisted | 5 s when stalled | +100 ms stalled; 100 ms healthy |
| record finalized → resources released | — | 100 ms healthy; stalled writer: 100 ms after it returns |
| healthy normal exit, root exit → capture frozen | measured cleanup interval (cleanup result − root exit observed) | +250 ms |
| reap-timeout transfer | 2 s from kill | +100 ms |
| reap-timeout transfer → logical record | 500 ms after transfer | +100 ms |

The wall deadline is observation-based with a fixed 50 ms grace on one
monotonic clock (§6.3). When the timer fires, the supervisor first checks for
exit, then commits enforcement, then kills. An exit seen after the commit does
not revoke it.

### 1.6 Persistence bounds

- One persistence thread per job and a 16-artefact queue. A full queue gives
  `PersistError{QueueFull}` and never blocks the job.
- Each artefact has a 5 s deadline from enqueue. A record is enqueued at
  finalization. On a miss, the thread is marked unavailable, every pending
  artefact fails with `Timeout`, and later ones fail with `WorkerUnavailable`.
  The job still finishes; in-memory report delivery is attempted separately.
- A rename that lands late is harmless. It never turns a reported
  `PersistError` into success, and the report never names a path it has not
  seen published.
- Settlement before report finalization ends by the last enqueue + 5 s. Size
  bounds: spec ≤ 256 KiB, record ≤ 64 KiB, `cleanup.log` ≤ 64 KiB,
  report ≤ 64 KiB + 12 KiB × planned executions (report contract §3.7).

### 1.7 Cancellation and force exit

- Cancellation records intent and stops admission. It never writes a verdict
  (§6.4).
- While the listener is installed, the first SIGINT/SIGTERM records intent
  unless the report is already frozen. The job attempts bounded persistence
  and delivery. The signal is re-raised only if cancellation wins exit-status
  precedence; delivery or internal failure suppresses orderly re-raising
  (`CancelSignals::finish`). A late cancellation can leave a completed primary
  verdict intact.
- Any later notification ≥ 100 ms after the first forces exit. Force exit
  takes the live-run registry lock (never held across creation or any blocking call),
  kills every registered run through its signalling capability, and
  terminates. It does not wait for a creation in progress (§3.4). It is the
  path where a CLI job intentionally abandons its final report; crashes can
  also prevent a report. Records already persisted remain.
- Dropping a public run future publishes cancellation and transfers any live
  ownership. Cleanup can publish a frozen `RootOutcome` through a ticket, but
  does not own a persistence queue or storage path. The engine enqueues records
  only after its executor returns; dropping that future does not arrange
  persistence (§§3.2 and 6.2). `cpf fsck` reports the schema of each run file present
  (`valid schema 1`); it does not diagnose an absent `record.json`.
  `cleanup.log` is the initial summary written by `JobPersistence` at record
  time; events after persistence settles are not appended. Dropping
  `JobPersistence` (`src/jobs/persist.rs`) while its worker is unavailable or
  still owes a result hands the worker's join handle to the cleanup service
  (`transfer_persistence` in `src/supervisor/cleanup.rs`) over an unbounded
  queue that never waits; each entry carries its worker's own credit, so there
  are at most 32, and none takes a cleanup slot. The thread's return value owns
  its credit, job-owner clone (lease and budget), queue with the metadata
  reservations of queued artefacts, and the timed-out artefact's reservation
  with its measured late bytes. Each poll joins only finished threads (as for
  transferred sink workers) and drops that value: the late bytes are
  committed, then everything else is released. Transferred workers count in
  `service_pending` and `transferred_workers` of `debug_counters` until then.
  A healthy idle worker is detached as before: closing its queue ends it
  without I/O and its credit returns at thread exit. A late rename after
  handover is not logged: the timed-out queue is unavailable and cleanup
  owns no persistence path.

## 2. Acceptance cases per OS

Legend:

- **V**: the cited assertions cover the stated case and are enabled by CI
  on that OS; this is a coverage statement, not a report of a CI run.
- **V (stub)**: verified with an injected stub, as the contract allows for
  that OS.
- **E**: engine or record/derivation logic only, without real supervised
  child processes (persistence tests can still use real files).
- **P**: partial evidence; the cell or accompanying text identifies what is
  checked and what remains unproved. **P/E** is partial engine-only evidence.
- **n/a**: the mechanism does not exist on that OS. The OS's own case replaces
  it.
- **pending**: needs Windows evidence once the Windows supervisor exists (see
  §3).

Test references use repository-relative source paths and exact function names;
`::name` inherits the last source path in that cell. They are not complete
libtest filter strings (unit tests also have module prefixes). `†` marks tests
behind the `storage-test-hooks` feature, which run in the `--all-features`
CI step on
every OS, subject to each OS gate. `‡` marks tests compiled only with
`debug_assertions` (the CI test profile). The suites
`tests/supervisor_linux.rs`, `tests/job_engine_linux.rs`,
`tests/session_linux.rs` and `tests/cli_signals.rs` are compiled for Linux and
macOS despite their names. Individual tests inside them may be limited to one
OS, as the columns show.

### 2.1 Supervisor acceptance suite

| # | Case | Mechanism | Tests | Linux | macOS | Windows |
|---|---|---|---|---|---|---|
| S1 | Root exits while a descendant holds stdout | post-exit group kill, drain to EOF | `tests/supervisor_linux.rs::pipes_drain_concurrently_and_descendant_writers_close`; `tests/pipes.rs::eof_waits_for_descendant_writer_after_root_exit` | V | V | pending (pipe-layer test runs) |
| S2 | Same, descendant holds stderr | same | `tests/supervisor_linux.rs::pipes_drain_concurrently_and_descendant_writers_close` (stderr iteration) | V | V | pending |
| S3 | Same, descendant holds neither, and is still killed | post-exit `killpg` + verification | `tests/supervisor_linux.rs::post_exit_kill_removes_live_group_members`, `::deadline_removes_three_descendants_and_confirms_the_group_empty` | V | V | pending |
| S4 | Escaped (`setsid`) descendant holds the pipe | drain deadline 500 ms, `NoEofBeforeDrainDeadline`, outside the cleanup claim | `tests/supervisor_linux.rs::capture_drain_deadline_stops_an_escaped_pipe_holder`, `::escaped_descendant_is_outside_the_cleanup_claim_and_is_killed_by_the_test` | V (drain → readers stopped, +100 ms) | V (same, ×4 allowance) | n/a (Job Objects) |
| S5 | Readers and runtime shut down within bounds | transfer cutoff, outcome delivered by the cleanup service | `tests/supervisor_linux.rs::capture_owner_and_runtime_can_drop_before_any_reader_poll`, `::dropping_capture_wait_freezes_at_transfer_cutoff_without_releasing_a_stalled_worker`, `::transfer_cutoff_starts_at_transfer_after_an_earlier_drain` | P (2 s post-drop cleanup watchdog; no contract runtime-shutdown bound; transfer cutoff +100 ms) | P (same; ×4 transfer allowance) | pending |
| S6 | Partial setup failure: pipe creation | injected `EMFILE` on the k-th pipe, partial setup released | `tests/supervisor_linux.rs::pipe_creation_failures_release_partial_setup_without_starting_children` †, `::session_pipe_creation_failures_release_cross_wiring_and_both_admissions` †; `tests/pipes.rs::injected_emfile_fails_only_the_kth_pipe_and_scope_restores_creation` † | V | V | pending |
| S7 | Partial setup failure: process creation | `CreateFailed{spawn, errno}`; ordinary setup releases resources, stalled rollback workers retain reservations until completion | `tests/supervisor_linux.rs::failed_creation_and_stdin_setup_leave_no_children_or_pipe_fds`, `::capture_worker_admission_and_setup_failures_return_all_reservations`, `::failed_capture_setup_transfers_a_sink_stalled_on_close` | V | V | pending |
| S8 | Partial setup failure: limit installation | phase pipe from `pre_exec` (Linux). Limits refused before setup (macOS). | `tests/supervisor_linux.rs::phase_pipe_distinguishes_limit_installation_from_exec_failure`, `::invalid_limits_fail_before_setup_without_creating_children` (Linux); `::macos_rejects_both_limit_kinds_before_workers_files_or_spawn` (macOS) | V | V (refusal) | pending (job commit limit) |
| S9 | Partial setup failure: Job Object | — | none | n/a | n/a | pending |
| S10 | Run inside an existing Job Object (compatible, and one that forbids nesting) | — | none | n/a | n/a | pending |
| S11 | Disk full in the capture sink | `WriteError(ENOSPC)`, child never blocked, budget loss kept separate | `tests/supervisor_linux.rs::real_enospc_reports_write_loss_without_blocking_or_conflating_budget_loss` (Linux tmpfs/macOS HFS+ image, required in CI); `::sink_partial_writes_and_eintr_are_retried_and_failures_never_block_the_child`, `::budget_discard_does_not_mask_write_loss_and_events_stop_exactly_at_cap` (injected generic write errors) | V (real ENOSPC required in CI) | V (real ENOSPC required in CI) | pending |
| S12 | Record or report persistence failure, disk full | `PersistError::Io(ENOSPC)`, verdict, references and delivery kept | `tests/job_engine_linux.rs::real_enospc_record_and_report_failures_keep_verdict_refs_and_delivery` † (Linux tmpfs/macOS HFS+ image); `src/jobs/persist.rs::real_enospc_keeps_failed_references_and_worker_accepts_a_later_write` (Linux/macOS); `src/jobs/engine/tests.rs::stalled_persistence_bounds_w_times_out_the_queue_and_omits_terminal_path`, `::stalled_report_publication_bounds_s_and_keeps_w_warnings_frozen`, `::report_collision_fails_publication_without_rewriting_or_changing_verdict` | V (real ENOSPC required in CI) | V (real ENOSPC required in CI) | P/E (stall and collision; no ENOSPC) |
| S13 | Repeated-run leak check: 50 warm-up, then 5 × 100 | Quiescent boundaries: equal fds, threads, slots, credits, service occupancy and root zombies; owned-resource counter zero † | `tests/supervisor_linux.rs::mixed_runs_leave_stable_fds_threads_slots_and_no_zombies`, `::four_hundred_runs_leave_no_zombies_or_descriptors`, `::captured_runs_release_descriptors_threads_and_credits`; `tests/pipes.rs::no_fd_leak_after_100_create_use_drop_cycles` (Linux module) | V (mixed and captured runs; owned counter †) | V (same; threads via `PROC_PIDTASKINFO`) | pending (handle counts) |
| S14 | Real Ctrl-C during build, test and custom commands | see §2.4 | see §2.4 | V | V | pending |

S11's real-ENOSPC test first tries a suitably small Linux tmpfs supplied by
`CPF_TEST_TINY_FS` or `/dev/shm` (at most 64 MiB), then a private 1 MiB mount
using root or `sudo -n`. S12 uses the private mount directly. On macOS both
cases use a private 16 MiB writable HFS+ image (`hdiutil create`/`attach`),
with a bounded helper timeout and forced detach on drop; no root is needed.
The shared `one_mib` entry point retains Linux's size in its name; HFS+ gets
room for its filesystem metadata. Filling stops at an actual `ENOSPC`,
with total writes bounded at 64 MiB. `CPF_REQUIRE_TMPFS=1` makes acquisition
failure fail the test; Linux and macOS CI set it. Local runs without that
variable can print `SKIP real ENOSPC`. Windows engine persistence tests cover
stalls (with an injected 1 s deadline) and publication collisions, not ENOSPC.
Pipe/fd failure seams run unconditionally in all-features tests, without a
skip branch.

S5 retains its two-second ticket/cleanup watchdog after runtime drop; it does
not time runtime destruction itself. §§5 and 10.1 define reader stop, transfer
cutoff and resource-release bounds, but give no independent runtime-shutdown
bound to substitute for that watchdog. The separately timed
reader-stop and transfer tests continue to assert their contract bounds.

S13's mixed-root and captured-run tests each use 50 warm-up + 5 × 100.
The separate pipe-only and four-hundred-run checks are complementary.
The feature-gated owned-resource counter is independent of admission and
credits: RAII guards track process-identity leases, shared capture state,
stdin pumps, relays and transcript workers. Outcome copies and tickets do
not count as physical ownership. The counter must be zero at every quiescent
boundary, alongside equal fds, threads and service counters and no root zombies.

### 2.2 Required cases (§10.3)

| # | Case | Mechanism | Tests | Linux | macOS | Windows |
|---|---|---|---|---|---|---|
| R1 | Immediate exit before, during and after observer registration | exit seen once and reaped once. pidfd valid for an unreaped child. kqueue `ESRCH`. | `tests/supervisor_linux.rs::immediate_exit_remains_observable_and_unreaped_until_wait`, `::exit_during_pidfd_registration_is_observed_and_reaped_once` † (Linux, child exits while creation is held before `pidfd_open`, and again between `pidfd_open` and AsyncFd registration); `tests/supervisor_linux.rs::exit_during_kqueue_registration_is_observed_and_reaped_once` † (macOS, queue created and EV_ADD held until the child is a zombie); `src/supervisor/root_macos.rs::exit_before_registration_takes_esrch_path_and_retains_status` (macOS) | V | V (ESRCH receipt or post-registration exit probe/event, exactly one observation and consuming wait) | pending |
| R2 | Forced observer failure → polling | Linux: injected `pidfd_open` `ENOSYS` (note) and `EMFILE` (`Register`). macOS: injected kqueue registration error. | `tests/supervisor_linux.rs::pidfd_open_enosys_and_emfile_fall_back_to_polling` † (Linux), `::exits_and_signals_with_both_observers`; `src/supervisor/root_macos.rs::registration_error_falls_back_and_finite_zero_reaps_back_off` (macOS) | V | V | n/a |
| R3 | `waitid`/`wait4` errors incl. injected `ECHILD` | §3.1.6: stop pid use, `Unknown`, transfer keeps the partial outcome | `src/supervisor/root_unix.rs::wait_errors_stop_pid_use_and_transfer_preserves_partial_outcome` | V | V | n/a |
| R4 | Worker pre-creation failure → `NotStarted{Worker}` | credit exhaustion (including exactly one free at job start) and injected thread-start failure. Every reservation returned. | `tests/supervisor_linux.rs::capture_worker_admission_and_setup_failures_return_all_reservations`, `::job_credit_reservation_with_one_free_refuses_without_reserving` † | V | V | pending |
| R5 | Limit failure in `pre_exec` (phase-tagged) vs other spawn failure (generic) | phase pipe | `tests/supervisor_linux.rs::phase_pipe_distinguishes_limit_installation_from_exec_failure` | V | n/a (no `pre_exec`, limits refused) | n/a |
| R6 | kqueue success vs error receipt | Live queue registration with a per-run injected error receipt; polling retains exit status | `src/supervisor/sys/macos.rs::receipts_are_acknowledgements_and_exit_events_are_strictly_validated`; `tests/supervisor_linux.rs::live_kqueue_error_receipt_falls_back_to_polling_and_preserves_exit_status` † | n/a | V (live registration receipt seam †) | n/a |
| R7 | `WAIT_FAILED` (injected) | — | none | n/a | n/a | pending |
| R8 | Cancellation before commit, during OS creation, after commit | Cancellation returns while the creation gate is held, including inside OS creation; cleanup kills and reaps after gate release (§1.2) | `tests/supervisor_linux.rs::cancel_before_commit_returns_while_creation_is_blocked` †, `::cancel_inside_os_creation_returns_then_cleanup_kills_and_reaps` †, `::cancel_after_commit_returns_before_creation_then_cleanup_reaps` †, `::cancellation_is_persistent_and_mid_run_cancellation_kills`; `tests/job_engine_linux.rs::cancel_during_executor_creation_keeps_the_runtime_responsive` †, `::cancel_inside_os_creation_persists_transferred_record_and_report` †, `::cancel_before_run_mkdir_keeps_the_runtime_responsive` †, `::cancel_before_recipe_completion_keeps_the_runtime_responsive` †; `src/jobs/engine/tests.rs::cancellation_before_admission_skips_without_allocating`, `::cancellation_after_admission_produces_a_not_started_record` | V | V | P/E (before/after admission only) |
| R9 | Cancellation and force exit during a blocked creation (injected) | Cancellation returns during committed creation for roots and connected pairs. Registry primitive kills an existing root without waiting for gated creation; real second SIGINT terminates cpf while its sole OS creation remains held, then the harness closes its end of the seam, so the unregistered child exits without exec | `tests/supervisor_linux.rs::cancel_before_commit_returns_while_creation_is_blocked` †, `::force_exit_registry_primitive_does_not_wait_for_blocked_creation` † (before and after commit), `::session_cancel_and_drop_between_root_commits_release_both_roots` †, `::session_cancel_inside_os_creation_returns_then_cleanup_reaps_both` †; `tests/cli_signals.rs::second_sigint_forces_exit_while_os_creation_is_still_held` † | V (real CLI force exit during pre_exec; at most one unregistered survivor) | V (same) | pending |
| R10 | Drop of the public future in every §3.2 state | Creation, Running, Terminating, observable-exit, reaped capture, Frozen transfer and Finalized no-op tested; dropped runs publish in-memory ticket outcomes (§3.2) | `tests/supervisor_linux.rs::drop_before_commit_never_creates_a_child` †, `::drop_after_commit_kills_and_transfers_the_created_child` †, `::dropped_owner_is_killed_and_reaped`, `::cancelled_wait_future_transfers_ownership`, `::dropped_exit_transfers_full_outcome_and_cloneable_ticket`, `::cancelled_wait_ticket_records_sigkill_even_before_first_poll`, `::dropping_a_terminating_future_preserves_the_trigger_and_notes_cancellation`, `::dropping_a_paused_wait_reconciles_an_already_exited_root`, `::dropping_capture_wait_freezes_at_transfer_cutoff_without_releasing_a_stalled_worker`, `::dropping_frozen_future_transfers_the_fixed_capture_and_records_cancellation` †, `::dropping_finalized_future_does_not_transfer_or_change_the_outcome` † | V | V | pending |
| R11 | Root exit with descendants holding stdout / stderr / neither | = S1–S3 | = S1–S3 | V | V | pending |
| R12 | Late `OutputBudget` after root exit is still a violation | budget counts observed bytes until EOF | `tests/supervisor_linux.rs::capture_late_budget_breach_after_root_exit_is_still_a_violation` | V | V | pending |
| R13 | Timer and exit ready in one wakeup, both orders (injected delay) | exit check before enforcement | `tests/supervisor_linux.rs::timer_and_exit_ready_together_are_reconciled_in_both_selection_orders`; `src/supervisor/root_unix.rs::observed_wall_judgment_has_exact_grace_boundaries_and_keeps_enforcement` | V | V | pending |
| R14 | Pending overlapped read during cancellation; completion racing `CancelIoEx` | — | none | n/a | n/a | pending |
| R15 | `ConnectNamedPipe` pending and failing (injected) | — | none | n/a | n/a | pending |
| R16 | Permanently stalled sink keeps its reservation until the writer returns | `WriteStalled` is final while the writer remains gated through an independent completed run and repeated ticket reads; only teardown releases it | `tests/supervisor_linux.rs::stalled_sink_freezes_at_five_seconds_and_keeps_its_slot_and_credit`, `::full_capture_queue_does_not_delay_deadline` | V (stub; drain → frozen 5 s +100 ms †) | V (stub; same ×4 allowance †) | pending |
| R17 | Sink write error (Linux tmpfs, macOS HFS+ image) | = S11 | = S11 | V (real ENOSPC required in CI) | V (real ENOSPC required in CI) | pending |
| R18 | Report persistence stalled and failing | = S12 | = S12 | P (real ENOSPC required; stall tests use 1 s) | P (real ENOSPC required; stall tests use 1 s) | P/E (1 s stall and collision) |
| R19 | Event consumer stalled for 10 000 runs; terminal ordering by `seq` | bounded transport, drops counted, `JobFinished` after channel close | `tests/job_engine_linux.rs::ten_thousand_real_runs_with_stalled_events_keep_terminal_sequence_and_report` † (10 000 real runs, consumer never polled); `src/jobs/engine/tests.rs::bounded_events_sequence_gap_equation_terminal_barrier_and_output_opt_in`; `tests/supervisor_linux.rs::capture_flood_is_exact_and_unread_events_stay_bounded` | V | V | P/E (three fake runs for ordering; no 10 000-run Windows test) |
| R20 | Cleanup capacity and thread-credit exhaustion (5th admission refused) | 4 slots, 32 credits | `tests/supervisor_linux.rs::four_slots_bound_admission_and_release_after_reap`, `::init_is_idempotent_and_thread_credits_are_bounded_and_returned`, `::capture_worker_admission_and_setup_failures_return_all_reservations`; `tests/job_engine_linux.rs::fixed_control_and_output_threads_hold_no_worker_credits` † | V | V | pending |
| R21 | Transferred run: logical record within its bound while physical cleanup continues | Logical record ≤ transfer +500 ms +100 ms; pinned zombie at finalization, consuming wait and resources released later | `tests/supervisor_linux.rs::reap_timeouts_transfer_identity_and_ticket_gets_the_physical_outcome`, `::transferred_finished_capture_waits_for_root_facts_or_cutoff`, `::transferred_run_without_captures_publishes_at_cutoff`, `::reap_timeout_logical_record_is_bounded_while_the_root_remains_unreaped` †; `src/supervisor/root_macos.rs::transferred_slow_scan_yields_so_another_run_can_finish` | V (gated reap, logical and physical timestamps †) | V (same; slow-scan fairness also checked) | pending |
| R22 | Enumeration error, truncation, short `proc_pidinfo`, zombie members | §4.4 scanners | Linux: `src/supervisor/sys/linux_proc.rs::directory_enumeration_failure_never_confirms_cleanup`, `::malformed_truncated_and_unreadable_entries_fail_the_scan`, `::completed_scan_uses_fdinfo_numbering_excludes_root_zombies_and_escaped_members`. macOS: `src/supervisor/sys/macos_proc.rs::live_enumeration_with_a_one_pid_buffer_reports_truncation`, `::short_pidinfo_reply_during_a_scan_never_confirms_cleanup`, `::full_buffers_grow_without_reallocation_and_stop_at_cap`, `::info_requires_exact_size_and_clean_errno_except_for_disappearance`, `::malformed_counts_errors_and_empty_success_are_distinct`; `src/supervisor/root_macos.rs::zombie_lookup_includes_an_unreaped_descendant_but_does_not_count_it_live`, `::libproc_failures_are_unverified_and_keep_measurements_partial` | V (including directory enumeration failure and zombie members) | V | pending (job accounting query) |
| R23 | `/proc` prerequisite failures and `hidepid` (injected) → `Unverified` | mountinfo and fdinfo checks | `src/supervisor/sys/linux_proc.rs::prerequisites_never_confirm_an_unsupported_view`, `::stacked_mounts_validate_the_visible_mount_id_and_filtering`; `tests/supervisor_linux.rs::unverified_cleanup_marks_reaped_measurements_partial` | V | n/a | n/a |
| R24 | Zero-byte read completion and `ERROR_BROKEN_PIPE` as EOF | — | none at supervisor level | n/a | n/a | pending |
| R25 | Late persistence rename after `PersistError` | late rename never fills the success cell or changes the report; handover holds the credit, lease and metadata until thread exit, then counters return to baseline | `src/jobs/persist.rs::late_rename_keeps_timeout_and_never_fills_the_success_cell` †; `src/jobs/engine/tests.rs::late_persistence_rename_keeps_report_final_at_w_without_duplicate_warnings` †; `tests/job_engine_linux.rs::late_rename_handover_holds_credit_lease_and_metadata_until_worker_returns`, `::stalled_persistence_handover_holds_queued_reservations_until_worker_returns` † (both handovers keep the worker stalled while another transferred root is reaped and releases its slot); `::healthy_idle_persistence_returns_credit_without_cleanup_handover` † (cumulative handover count unchanged; credits returned within 100 ms at job end) | V (real supervised job and concurrent cleanup progress) | V (same, ×4 allowance) | E (no handover case) |
| R26 | Budget breach plus an independent write error, both reported | losses judged per kind | `tests/supervisor_linux.rs::budget_discard_does_not_mask_write_loss_and_events_stop_exactly_at_cap`, `::real_enospc_reports_write_loss_without_blocking_or_conflating_budget_loss` (Linux); `src/report/tests.rs::supervisor_adapter_fixtures`, `::classify_run_fixtures` | V | V | E (derivation) |
| R27 | `ACTIVE_PROCESS_ZERO` never delivered, still `Confirmed` by polling | — | none | n/a | n/a | pending |
| R28 | Address-space limit with a lower inherited hard limit | `Note::LimitLowered` | `tests/supervisor_linux.rs::inherited_address_space_hard_limit_is_respected` | V | n/a (refused) | n/a |
| R29 | `Connected` variants and `deadline_group` rejected where unsupported | `Connected` stdin/stdout/stderr refused by plain root API before worker creation, with admission resources returned; `deadline_group` is absent from `ProgramSpec` and cannot be requested | `tests/session_linux.rs::connected_plain_runs_are_unsupported` | P (all endpoints V; deadline_group API absent) | P (all endpoints V; deadline_group API absent) | pending |
| R30 | Real Ctrl-C and second Ctrl-C | see §2.4 | see §2.4 | V | V | pending |

### 2.3 Bounds and invariants (§10.1, §10.2)

| # | Case | Tests | Linux | macOS | Windows |
|---|---|---|---|---|---|
| T1 | timer → kill ≤ 50 ms; kill → root exit ≤ 250 ms | `tests/supervisor_linux.rs::deadline_kills_hold_within_the_policy_bounds` | V (×2 on hosted CI) | V (×4) | pending |
| T2 | root exit → cleanup result ≤ 1 s + 50 ms, also with a scan delayed past the deadline | `tests/supervisor_linux.rs::root_exit_to_cleanup_result_meets_the_cleanup_deadline` †, `::cleanup_deadline_during_rescan_keeps_last_live_count` † (harness-owned live member joins the pinned group after the post-exit kill; next scan entry delayed); scanner semantics: `src/supervisor/sys/linux_proc.rs::scan_cut_by_cleanup_deadline_uses_only_the_last_completed_live_count`, `::slow_scan_distinguishes_the_reap_deadline_from_the_cleanup_deadline`; `src/supervisor/sys/macos_proc.rs::newer_scan_cut_by_the_cleanup_deadline_keeps_the_last_live_count`, `::completed_live_scan_at_deadline_differs_from_no_completed_scan` | V (live non-empty group and last completed count) | V (same; 1 s +50 ms ×4 allowance) | pending |
| T3 | Healthy milestones: cleanup → drain 50 ms; drain → readers stopped 100 ms; readers stopped → frozen 100 ms; frozen → finalized 100 ms; finalized → persisted and finalized → released 100 ms each | `tests/job_engine_linux.rs::healthy_run_milestones_meet_each_bound_over_five_runs` † (real engine, real persistence); wall observation only: `tests/supervisor_linux.rs::short_sleep_exits_without_enforcement_and_records_observed_wall` | V (×2 on hosted CI) | V (×4) | pending |
| T4 | Consuming-wait retries bounded by the reap deadline, then transfer | `tests/supervisor_linux.rs::reap_timeouts_transfer_identity_and_ticket_gets_the_physical_outcome`, `::slow_cleanup_transfers_at_the_original_kill_reap_deadline` (Linux), `::post_exit_deadline_and_cancellation_do_not_add_late_trigger_notes`; `src/supervisor/root_macos.rs::zero_wait4_after_note_exit_retries_without_another_notification` | V | V | n/a |
| T5 | Escaped pipe: contract drain 500 ms +100 ms | = S4 | V (drain → readers stopped, +100 ms †) | V (same; allowance ×4) | n/a |
| T6 | Stalled sink: contract drain → frozen 5 s +100 ms | = R16 | V (drain → frozen, +100 ms †) | V (same; allowance ×4) | pending |
| T7 | Reap-timeout path: contract logical record ≤ transfer +500 ms +100 ms | `tests/supervisor_linux.rs::transferred_run_without_captures_publishes_at_cutoff`, `::transferred_finished_capture_waits_for_root_facts_or_cutoff`, `::reap_timeout_logical_record_is_bounded_while_the_root_remains_unreaped` † | V (transfer → record +100 ms †) | V (same; allowance ×4) | pending |
| T8 | Late-trigger and cancellation timing notes | `tests/supervisor_linux.rs::cancellation_after_observable_exit_is_only_a_note`, `::deadline_before_observation_after_cancellation_is_a_late_trigger`, `::cancellation_after_a_deadline_kill_and_exit_observation_is_not_a_late_trigger` | V | V | pending |
| T9 | Healthy normal exit: root exit → frozen ≤ cleanup interval +250 ms | `tests/job_engine_linux.rs::healthy_run_milestones_meet_each_bound_over_five_runs` † | V (aggregate cleanup interval +250 ms) | V (same, ×4 allowance) | pending |
| T10 | Stalled persistence: finalized → persisted/error 5 s +100 ms | `src/jobs/engine/tests.rs::stalled_persistence_bounds_w_times_out_the_queue_and_omits_terminal_path`, `::stalled_report_publication_bounds_s_and_keeps_w_warnings_frozen`, `::late_persistence_rename_keeps_report_final_at_w_without_duplicate_warnings` † | P/E (1 s injections; 5 s rename test has 8 s watchdog) | P/E (same) | P/E (same) |
| T11 | Stalled writer: resources released ≤100 ms after return | `tests/supervisor_linux.rs::stalled_sink_freezes_at_five_seconds_and_keeps_its_slot_and_credit` | V (100 ms from gate release) | V (same, ×4 allowance) | pending |
| I1 | Fixture readiness `READY <pid> <start-id>`, blocking acknowledgement, 5 s timeout kills the group | `crates/cpf-fixture/tests/fixture.rs::ready_pins_tree_identities_and_group_kill_removes_them`, `::nested_spawn_and_repeated_ready_do_not_steal_acks`, `::unacknowledged_ready_times_out_before_executing_next_step`, `::missing_ready_kills_containment_and_fails_the_test` | V | V | V (fixture only) |
| I2 | "Gone" = no live identity (`ESRCH`, other start-id, or zombie) | `crates/cpf-fixture/tests/fixture.rs::killed_root_is_gone_before_it_is_reaped`; identity checks throughout `tests/supervisor_linux.rs` | V | V | pending |
| I3 | Leak checks | = S13 | V (owned counter †) | V (same) | pending |
| I4 | Memory: peak RSS in [N, N + 16 MiB + baseline], bytes | `tests/supervisor_linux.rs::wait4_reports_resident_memory_in_bytes`; `crates/cpf-fixture/tests/fixture.rs::touched_memory_stays_alive_until_killed` | V | V | pending (working set, job commit) |
| I5 | CPU time in µs, declared scope root + waited descendants | `tests/supervisor_linux.rs::busy_fixture_reports_cpu_microseconds`, `::busy_waited_descendant_reports_cpu_microseconds_within_wall`, `::measurements_select_deduplicate_and_report_unsupported_metrics`, `::unreaped_measurements_remain_absent_and_frozen_after_late_reap` | V (root and serial waited descendant; CPU µs ≤ wall µs) | V (same) | pending |
| I6 | Cap boundaries 4095/4096/4097, both streams, all sink and exceed policies | `tests/supervisor_linux.rs::capture_budget_boundaries_clip_both_streams_for_every_policy` | V | V | pending |
| I7 | Flood: 64 MiB on both streams concurrently | `tests/supervisor_linux.rs::capture_flood_is_exact_and_unread_events_stay_bounded`; `crates/cpf-fixture/tests/fixture.rs::flood_drains_both_streams_concurrently` | V | V | P (fixture only: 1 MiB per stream) |
| I8 | Start-up: SIGCHLD default, no `SA_NOCLDWAIT`, inherited fds CLOEXEC | `src/supervisor/sys/linux.rs::init_preserves_sigchld_default_and_marks_inherited_fd`; `src/supervisor/sys/macos.rs::startup_sanitizes_inherited_descriptors`; `tests/supervisor_linux.rs::inherited_sigchld_ignore_is_reset_before_spawn` | V | V | pending |
| I9 | Exit codes and signals through both observers; cwd, complete env, file stdin applied | `tests/supervisor_linux.rs::exits_and_signals_with_both_observers`, `::creation_applies_cwd_complete_env_and_file_stdin` | V | V | pending |

Fixed-thread accounting (§3.3) is also checked by
`tests/job_engine_linux.rs::fixed_control_and_output_threads_hold_no_worker_credits` †:
`cpf-control` stays installed, actual `cpf-stdout`/`cpf-stderr` writes acknowledge
readiness, and credits count only cleanup plus the two job workers before and
after a real execution. Healthy persistence never enters cleanup (including
transient handovers, checked with a cumulative feature-gated counter); stalled
persistence retains its credit while an independent transferred root finishes.

### 2.4 Signals, cancellation and force exit (§8.2)

| # | Case | Tests | Linux | macOS | Windows |
|---|---|---|---|---|---|
| C1 | Real SIGINT/SIGTERM during build, test, run, cmd, repeat, `for_each_test`, hooks: intent recorded, report persisted/delivered before re-raise; identities gone within 2 s after CLI exit (after-test primary verdict stays passed) | `tests/cli_signals.rs::signals_during_every_job_persist_and_deliver_before_reraise` | V | V | pending (console Ctrl-C/close) |
| C2 | Second signal forces exit after killing every live job tree | `tests/cli_signals.rs::second_sigint_kills_every_live_job_tree_before_exiting` †, `::second_sigint_forces_exit_while_os_creation_is_still_held` †; `tests/cli_run.rs::second_sigint_forces_exit_during_console_drain`; `src/jobs/engine/cancel.rs::force_exit_does_not_wait_for_the_verdict_lock` | V | V | pending |
| C3 | Signals during planning refuse without admitting a job | `tests/cli_signals.rs::planning_signals_refuse_without_admitting_a_job` † | V | V | pending |
| C4 | Signals between iterations skip the remaining admissions and keep completed runs | `tests/cli_signals.rs::signals_between_repeat_and_test_iterations_skip_all_remaining_admissions` †, `::signals_in_later_iterations_preserve_completed_runs_and_skip_the_tail`; `tests/cli_cmd.rs::sigint_between_iterations_delivers_cancelled_coverage_before_signal_death` † | V | V | pending |
| C5 | Signals after delivery leave the frozen report; read-only commands keep default actions; listener restored | `tests/cli_signals.rs::signals_after_delivery_leave_the_frozen_report_and_status_unchanged_unless_forced` †, `::read_only_commands_and_switch_before_hooks_retain_default_signal_actions` †, `::listener_drop_restores_defaults_and_reinstallation_has_no_pending_intent` | P (raw run skips single post-delivery signal) | P (same) | pending |
| C6 | Delivery failure outranks signal cancellation | `tests/cli_signals.rs::delivery_failure_outranks_real_signal_cancellation_for_every_machine_job` | V | V | pending |

The signal tests send signals to the `cpf` pid, not to a terminal foreground
group. Children run in their own process groups, so this is the same input for
`cpf`.

### 2.5 Connected runs

The Unix session API implements relay and direct modes using the same root
coordination. This extends execution §9a's provisional relay-only description:
plain `Connected` endpoints are refused, while `SessionSpec` supplies the pair
and connection ownership. Direct mode rejects transcripts and idle monitoring;
relay mode supports both. This is a supervisor API, not evidence of CLI
interactive judging.

Each function below is in `tests/session_linux.rs`, except fixture 16 in
`src/supervisor/session/record.rs`. The names are exact; fixtures 9–11 require
debug assertions. Windows runs only the record logic; `run_session` otherwise
returns `Unsupported` before admission (`src/supervisor/session.rs`).

| Fixture | Evidence / test function | Linux | macOS | Windows |
|---|---|---|---|---|
| 1: wall limit and provenance | `fixture_01_wall_limit_and_provenance` | V | V | pending |
| 2: sink gone | `fixture_02_sink_gone_reaches_writer_as_sigpipe` | V | V | pending (`ERROR_NO_DATA`) |
| 3: half-close while root alive | `fixture_03_half_close_while_root_alive` | V | V | pending |
| 4: draining destination receives all bytes | `fixture_04_draining_destination_receives_every_byte` | V | V | pending |
| 5: slow destination, loss accounting | `fixture_05_slow_destination_and_exact_loss_accounting` | V | V | pending |
| 6: inherited stdout through drain | `fixture_06_inherited_stdout_survives_until_transport_drain` | V | V | pending |
| 7: bidirectional writes stop at limit | `fixture_07_large_bidirectional_writes_end_at_earliest_limit` | V | V | pending |
| 8: stderr flood preserves forwarding | `fixture_08_stderr_flood_does_not_stall_forwarding` | V | V | pending |
| 9: blocked transcript queue | `fixture_09_blocked_transcript_queue_does_not_block_forwarding` ‡ | V (stub: finite delay) | V (stub: finite delay) | pending |
| 10: transcript write failure | `fixture_10_permanent_transcript_failure_is_reported_in_outcome` ‡ | V (stub) | V (stub) | pending |
| 11: cancellation with a partial forwarded prefix | `fixture_11_cancellation_records_the_exact_partial_forward` ‡ | P (synthetic pause, no kernel short write) | P (same) | pending |
| 12: metadata saturation | `fixture_12_metadata_saturation_is_bounded` | V | V | pending |
| 13: both exits, final drain | `fixture_13_both_exits_then_bounded_final_drain` | V (500 ms–3 s elapsed) | V (same) | pending |
| 14: peer allowance, both directions | `fixture_14_peer_allowance_in_both_directions` | V | V | pending |
| 15: cancellation during blocked forward | `fixture_15_cancel_during_blocked_forward` | V | V | pending |
| 16: same-wakeup selection | `selection_rechecks_prerequisites_and_coalesces_stops`, `exit_precedes_timer_and_cleanup_becomes_eligible_immediately` | E (record logic) | E (record logic) | E (record logic) |
| 17: exchange latency | `fixture_17_ten_thousand_exchange_latency_ci` | P (10 000; 30 s total ceiling per mode) | P (same) | pending |
| 18: 50 warm-up + 5 × 100 sessions | `fixture_18_fifty_warmup_then_five_hundred_sessions_release_all_resources` | V (alternating relay/direct; owned counter †) | V (same) | pending |

Fixture 17 prints timing measurements; it does not assert a 35 µs target.
`tests/session_linux.rs::fixture_17_tiny_exchange_latency_benchmark` uses
100 000 exchanges per mode on Linux and macOS, is runnable with `--ignored`,
and prints the same measurements. CI deliberately runs 10 000 exchanges per
mode; the full benchmark is excluded from the checked-in CI commands.
Both test passes run on every OS, and the only measurements (Linux: relay
31–79 µs, direct 14–27 µs per round trip) give no basis for a hosted macOS
figure, so 100 000 exchanges could add well over 30 s per OS. No particular hosted-runner latency is a guarantee.

Additional evidence in `tests/session_linux.rs`:

| Case | Exact test function | Linux | macOS | Windows |
|---|---|---|---|---|
| Native/polling observers and scoped accounting, both modes | `both_modes_use_native_observers_or_bounded_polling_and_scoped_accounting` | V | V | pending |
| Descendant identities gone on finish, cancel and drop, both modes | `both_modes_reap_roots_and_remove_live_descendant_identities_on_finish_cancel_and_drop` | V | V | pending |
| Direction independence | `one_blocked_direction_does_not_stall_the_reverse_direction` | V | V | pending |
| Idle watchdog | `idle_stops_both_sleeping_roots_in_role_order` | V | V | pending |
| Pair admission and worker rollback | `pair_admission_failure_releases_every_reservation`, `pair_worker_credit_failure_creates_no_workers_or_roots`, `second_worker_failure_rolls_back_prepared_peer_without_starting` | V | V | pending |
| Recovery record before peer teardown | `unobservable_root_recovery_records_infrastructure_before_stopping_peer` | V | V | pending |
| Transcript bytes and offsets | `transcript_raw_bytes_offsets_and_root_events_are_complete` | V | V | pending |
| Transcript path safety | `every_transcript_path_rejects_existing_files_fifos_and_symlinks_before_admission` | V | V | pending |
| Dialogue cap attribution | `dialogue_cap_is_attributed_to_its_writer` | V | V | pending |
| Direct-mode unsupported requests | `direct_mode_transcript_and_idle_requests_are_rejected_before_start` | V (refusal) | V (refusal) | pending |

### 2.6 Coverage limits

The rows describe assertions enabled by `.github/workflows/ci.yml`'s default
and all-features test steps on Linux, macOS and Windows. Ignored benchmarks are not unconditional CI evidence. Linux CI requires
the real tmpfs branches with `CPF_REQUIRE_TMPFS=1`. Engine-only,
fixture-only and partial checks cannot establish a working Windows supervisor.
There is no cited real uninterruptible-sleep fixture for §10.1's opt-in Linux
reap-timeout case: the tests inject unavailable observation/reaping instead.
T10 remains partial because the persistence stall injections do not establish
the exact contract deadline and allowance.

## 3. Windows: current limitation

The Windows supervisor is **not implemented**. Job execution (`cpf build`,
`cpf test`, `cpf run`, `cpf cmd`) is refused **before admission** with class
`infrastructure` and exit status 10. Inside a workspace, planning/configuration
errors can be reported first; valid execution requests reach the platform
refusal in `job_command::plan_invocation`. No job directory, run or child is
created. `cpf switch` hooks warn and do not run. Tests:
`tests/cli_build.rs::unsupported_platform_is_infrastructure_before_admission`,
`tests/cli_test_run.rs::windows_refuses_test_execution_before_admission`,
`tests/cli_run.rs::windows_refuses_raw_run_before_workspace_admission`,
`tests/cli_cmd.rs::windows_refuses_execution_before_workspace_discovery`,
`tests/cli_selection.rs::trusted_switch_warns_without_running_hooks_on_windows`.

These parts do exist and are tested on Windows: the named-pipe layer
(`src/supervisor/sys/windows.rs`, portable tests in `tests/pipes.rs`), the
job engine with test executors (rows marked E), report derivation, storage,
and the `cpf-fixture` harness.

Cases that need Windows evidence when the supervisor is implemented (§4.3,
§10.3):

- **Job Objects:** creation-time membership through
  `PROC_THREAD_ATTRIBUTE_JOB_LIST`, `KILL_ON_JOB_CLOSE`, a compatible outer
  job and one that forbids nesting (S10), job setup failure (S9),
  `ACTIVE_PROCESS_ZERO` never delivered but `Confirmed` by polling (R27), and
  job accounting as the cleanup truth.
- **Handles:** `PROC_THREAD_ATTRIBUTE_HANDLE_LIST` inheritance, a
  non-inheritable job handle, the waiter thread and `WAIT_FAILED` (R7), and
  handle-count leak checks (S13).
- **Overlapped I/O:** completion ownership for every operation, a pending
  read during cancellation and a completion racing `CancelIoEx` (R14),
  `ConnectNamedPipe` pending and failing (R15), zero-byte completions and
  `ERROR_BROKEN_PIPE` as EOF (R24).
- **Measurements and limits:** peak working set, job commit in
  [N, N + 32 MiB + baseline], the `JobCommit` limit.
- **Console:** Ctrl-C and console close → exit 6. Console close before
  delivery is an open issue.
- **Connected runs** on Windows, including the `ERROR_NO_DATA` variant.

Until implemented, these supervisor rows remain pending. The refusal and
portable-layer behaviors above are the Windows behavior currently evidenced.

## 4. Residual risks and caveats

- **Unregistered child on force exit (Unix).** Force exit does not wait for a
  creation in progress. A child whose `spawn` is still running, or has
  returned but is not yet in the registry, can survive force exit for an
  unbounded time. There is at most one per job, because each job has one
  creation thread (§3.4, §11). R9 combines a real second SIGINT with a
  socket-held pre_exec. It verifies prompt force-exit status while the only
  unregistered child is still held, then closes the seam so that child exits
  without exec. The registry-primitive and C2 tree tests also
  verify that registered runs are killed.
- **Escapes and third-party reaping.** Cleanup verification observes the
  process group. Descendants that call `setsid`/`setpgid`, or that are reaped
  by someone else, are outside the claim (S4). This is not a sandbox.
- **macOS cleanup verification.** It relies on libproc
  (`proc_listpids(PROC_PGRP_ONLY)`, `proc_pidinfo(PROC_PIDTBSDINFO, 1)`).
  `PROC_PGRP_ONLY` is supplied locally as SDK constant 2, because the pinned
  `libc` omits it. A scan is not an atomic snapshot. The kernel calls cannot
  be pre-empted, so deadlines are checked around them, not during them. Groups
  filling the 64 Ki-pid buffer are `Truncated`. Validation targets the macOS
  release and architecture selected by the moving `macos-latest` label; the workflow
  does not pin either. The capability probe is informative; scans validate
  their own calls and classify failures, including `ENOSYS`/`ENOTSUP` as
  `Unverified{Unsupported}`, which does not block a pass. Peak RSS on
  Darwin is the `wait4` high-water mark in bytes, not a simultaneous tree peak.
- **Partial-forward test uses a barrier.** Fixture 11 checks the exact
  4096-byte forwarded prefix with a debug-only relay pause
  (`relay_pause_after`), because Darwin cannot resize pipes. A real
  kernel short write in the relay is not produced. The bytes and bounds
  asserted are the same, but the trigger is synthetic on both OSes.
- **Real ENOSPC needs a small tmpfs.** Linux CI requires acquisition for S11
  and S12 using `CPF_REQUIRE_TMPFS=1`; local runs can still skip if it is unset.
  On macOS the sink error is always a stub.
- **Debug-build stack depth.** The real-engine integration harnesses use an
  explicit 8 MiB thread stack (`tests/job_engine_linux.rs`,
  `tests/ready_jobs_linux.rs`), and the CLI boxes the engine future on its main
  thread. No test bounds the poll-frame size or validates smaller stacks;
  the CLI does not itself set its main-thread stack size.
- **Timing on hosted runners.** Contract allowances assume scheduling behavior
  (×2 on hosted Linux CI, ×4 on macOS and Windows). Reactor stalls can exceed the fixed 50 ms grace;
  polling's 10 ms backoff cap does not bound scheduling delay. Both can produce
  a violation for a program that finished in time. `Timing.observer` and the
  wall method identify the observation mechanism. P rows identify tests whose
  bounds are weaker than the contract.
- **Unverified cleanup passes.** A pass does not imply verified cleanup. The
  summary says "cleanup not verified" when it is not, and measurements stay
  `Partial` unless cleanup is `Confirmed`.
- **Raw `cpf run` exit.** At exit, physical cleanup still pending (a stalled
  writer, an unreaped root after `ReapTimeout`) is left to process teardown.
  An unreaped root is reparented to init, and the record says so (§8.1).
