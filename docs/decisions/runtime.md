# Runtime and supervisor decisions

Status: current for the CLI foundation. This record states how `cpf` runs
and supervises child processes, why, what is guaranteed on each operating
system, and which test proves each acceptance case. The normative rules are
in the [execution contract](../contracts/execution.md) (cited as §n). The
Darwin details are in the [macOS supervisor design](../design-macos-supervisor.md).
Where this record and the contract differ, the contract wins and this record
is out of date.

Platforms: Linux (kernel ≥ 5.4) and macOS (13+) are supported. CI validates
Linux on `ubuntu-latest` and macOS on `macos-latest` (arm64, macOS 26).
Older macOS releases and Intel Macs are not validated separately. The Windows
supervisor is **not implemented**. See [Windows](#windows-current-limitation).

## 1. Decisions

### 1.1 Runtime model

The design left open a choice between Tokio and a small thread-based job
runner. The decision is **Tokio, single-threaded per job, plus a fixed set of
dedicated OS threads for every blocking step.**

| Element | Decision |
|---|---|
| Job executor | One Tokio **current-thread** runtime per job command, built in `job_command` and driven with `block_on` on the CLI main thread. Hook-only jobs (`cpf switch`) use their own current-thread runtime in the same way. No multi-threaded scheduler drives a job. |
| Async work | Pipe readers (async tasks on non-blocking pipe ends), exit observers (pidfd or kqueue registered with `AsyncFd`), deadline and backoff timers, cancellation, session relays and the event channel. |
| Blocking work | Never inside an async poll. Each blocking step runs on a dedicated thread (table 1.2). |
| Tokio features | `process` is off. `tokio::process` and `std::process::Child::{wait, try_wait, kill}` are not used, and CI checks that no crate enables the feature (§3.1.3). Only `supervisor::sys` waits on a child, by pid. It never waits on `-1` or on a group. |

Why:

- The asynchronous part of supervision is waiting on readiness: exit
  notifications, pipe data, timers and cancellation. One reactor thread
  handles this without locks between runs of the same job.
- A single thread makes "several events ready in one wakeup" a defined case.
  The supervisor reconciles timer and exit with an explicit exit check
  (§6.3), so selection order never decides a verdict.
- Creation, file I/O and persistence can block for an unbounded time (slow
  disks, FUSE, a stalled `rename`). On dedicated threads they cannot stall
  the reactor, and their number is bounded and counted (32 thread credits,
  §3.3). Tokio's blocking pool would hide that count.

### 1.2 Threads

All workers a run needs are created **before** its child exists, so a run
never needs a new thread to clean up. Creation failure is
`NotStarted{Worker}`, and every credit is returned.

| Thread | Scope | Created | Does |
|---|---|---|---|
| Cleanup service (`cpf-cleanup`) | process | at `init_process`, before any job | owns transferred runs. Polls and reaps their roots with backoff, receives completions from transferred workers, finalizes their records at the cutoff. Never blocks. It is the recovery route for every observer failure. |
| Creation (`cpf-create`) | job | at job start, holds 1 credit | FIFO queue of blocking setup: run-directory creation, recipe resolution (deferred program completion, staging), opening stdin and sink files, pipe creation, the `spawn` call, and post-build validation (§3.4). |
| Persistence (`cpf-persistence`) | job | at job start, holds 1 credit | temp file + rename for spec, record, `cleanup.log` and report, fed by a 16-entry queue (§6.2). |
| Sink worker (`cpf-sink`) | run | at admission | blocking writes of captured bytes to the run directory (§5.1). |
| Console forwarder (`cpf-console`), stdin forwarder (`cpf-stdin`) | run, raw `cpf run` only | at admission | §8.1. |
| Transcript writers (`cpf-transcript-*`) | session | at pair admission | bounded transcript files of connected runs. |
| Control (`cpf-control`) | process | with the signal listener | turns SIGINT/SIGTERM from a self-pipe into cancellation or force exit (§8.2). Never the executor or the creation thread. |
| Output (`cpf-stdout`, `cpf-stderr`) | process | per command | machine-output delivery and progress lines, kept off the executor. |

Fixed process-wide bounds (§3.3): 4 cleanup slots (so at most 4 admitted
runs, 2 for a connected pair) and 32 thread credits. Unix observers use no
thread.

**Creation thread.** Before this thread existed, setup ran synchronously inside
the async poll. That broke §3.4 because a blocked `open` or `spawn` stalled
cancellation. Now one atomic per run orders *commit* against *cancel*:

- Cancellation before commit returns at once with
  `NotStarted{CancelledBeforeStart}`, and no child is created.
- Cancellation after commit does not wait. The creation thread registers the
  child under the registry lock as soon as `spawn` returns, then runs the
  kill path.
- Run-directory creation and recipe resolution are interrupted at step
  boundaries. Post-build validation is awaited, because it decides the build
  result.

The deadline timer starts when creation returns. `setup_duration` is recorded
and lies outside the deadline guarantee.

### 1.3 Exit observers

| OS | Primary observer | Fallback | Notes |
|---|---|---|---|
| Linux | `pidfd_open(pid, 0)` registered with `AsyncFd` | Any `pidfd_open` error: threadless polling with `waitid(P_PID, WEXITED \| WNOHANG \| WNOWAIT)`, backoff 1, 2, 4 … 10 ms. `ENOSYS`/`EPERM` add `Note::ObserverFallback`; other errnos add `SupervisorError::Register`. | Polling adds ≤ 10 ms to observation and is recorded in `Timing.observer`. |
| macOS | One CLOEXEC kqueue per run, `EVFILT_PROC`/`NOTE_EXIT` with `EV_ADD \| EV_RECEIPT`, registered with `AsyncFd` | Error receipt: `Register` + polling. `ESRCH` receipt: the root has already exited (exit latched). | Exit events are accepted only with a matching pid, filter and `NOTE_EXIT`. A consumed event stays latched across cancellation of the observer future. Darwin lacks `pipe2`/`kqueue1`, so a creation mutex serializes pipe/kqueue creation + `FD_CLOEXEC` with every spawn. |
| Windows | Planned: waiter thread on the process handle (§4.3) | Planned: cleanup-service polling of `WaitForSingleObject(process, 0)` | Not implemented. |

On Unix the root is spawned with `process_group(0)`, so the root's pid is the
pgid. `killpg` is allowed only while the root is known unreaped. The flag is
cleared under the registry lock **before** the consuming `wait4`. Any wait
error that may lose identity protection (`ECHILD`, or anything other than
`EINTR`) clears it at once and yields status `Unknown` (§3.1.6). A `wait4`
returning 0 right after readiness is retried with backoff up to the reap
deadline. After that the still-owned root is transferred and the cleanup
service keeps polling. No unreaped root is ever released.

### 1.4 Cleanup verification

`Confirmed` means only this: the stated algorithm, run after the group kill,
observed no live (non-zombie) member of the group other than the root. It is
not proof against `setsid`/`setpgid` escapes and says nothing about I/O
completion. Scans repeat with backoff until `Confirmed` or the 1 s cleanup
deadline. Only a completed scan of a supported view can confirm. `Unverified`
does not block a pass. `Incomplete` does.

| OS | Method | Becomes `Unverified` when |
|---|---|---|
| Linux | Iterate every numeric `/proc` entry and parse `state`/`pgrp` from `/proc/<pid>/stat` into one reused 4 KiB buffer. The group id is the root's number **in this procfs instance**, from the `Pid:` field of the pidfd's `fdinfo`. No PID-namespace equality is assumed. | no pidfd (polling fallback) or `Pid: 0` → `NamespaceUnknown`. `hidepid`/unparseable mount options → `ProcFiltered`. Oversized/unparseable record → `Malformed`. I/O error → `Io`. Deadline mid-scan with no completed scan → `ScanIncomplete`. |
| macOS | `proc_listpids(PROC_PGRP_ONLY, pgid)` into a buffer of up to 64 Ki pids reserved per root, then `proc_pidinfo(PROC_PIDTBSDINFO, arg = 1)` per pid. `errno` is cleared before and checked after each call. Only exact-size replies are read, and membership and liveness are rechecked from `pbi_pgid`/`pbi_status`. | full 64 Ki buffer → `Truncated`. Bad byte count or negative pid → `Malformed`. Info error or short reply → `InfoFailed`. `ENOSYS`/`ENOTSUP` → `Unsupported`. List error → `Io`. |
| Windows | Planned: `JobObjectBasicAccountingInformation.ActiveProcesses == 0`, polled and queried once more at the deadline. The completion-port message is only a wakeup. | not implemented |

The two scanners resolve the deadline in the same way. When the deadline cuts
a rescan short, the most recent completed scan that found live members gives
the `Incomplete` survivor count. A rescan that was cut short has not seen those
members leave. An earlier (or equal) reap deadline takes precedence, and the
unfinished scan moves with the transferred root, so the cleanup service never
sleeps or loops on one run.

### 1.5 Deadlines and test allowances

Policy deadlines are fixed (§5.5) and are never scaled. Tests add an
**allowance** for scheduling on hosted runners: the Linux value as listed, and
×4 on macOS (and on Windows when it exists). The allowance is an assumption
about CI hosts, not part of the guarantee.

| Path (§10.1) | Policy deadline | Test allowance (Linux) |
|---|---|---|
| timer fire → kill requested | — | 50 ms |
| kill → root exit observed, normal tree | — | 250 ms |
| root exit → cleanup result | 1 s | +50 ms |
| cleanup result → drain start, immediate reap | — | 50 ms |
| consuming-wait retries | reap deadline 2 s from kill (or from exit) | +50 ms, then transfer (+100 ms) |
| drain start → readers stopped, pipes closed | — | 100 ms |
| pipe held outside the group | drain 500 ms | +100 ms, `NoEofBeforeDrainDeadline` |
| readers stopped → capture frozen | — | 100 ms |
| stalled sink | sink finalisation 5 s | +100 ms, `WriteStalled` |
| capture frozen → record finalized | — | 100 ms |
| record finalized → persisted / released | persistence 5 s when stalled | 100 ms healthy |
| reap-timeout transfer → logical record | drain 500 ms after transfer | +100 ms |

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
  The job still finishes and delivers its in-memory report.
- A rename that lands late is harmless. It never turns a reported
  `PersistError` into success, and the report never names a path it has not
  seen published.
- Settlement before report finalization ends by the last enqueue + 5 s. Size
  bounds: spec ≤ 256 KiB, record ≤ 64 KiB, `cleanup.log` ≤ 64 KiB, report per
  report contract §3.7.

### 1.7 Cancellation and force exit

- Cancellation records intent and stops admission. It never writes a verdict
  (§6.4).
- The first SIGINT/SIGTERM cancels the current job. The report is persisted
  and delivered, and then the signal is re-raised.
- A second notification ≥ 100 ms later forces exit. Force exit takes the
  live-run registry lock (never held across creation or any blocking call),
  kills every registered run through its signalling capability, and
  terminates. It does not wait for a creation in progress (§3.4). It is the
  one path where the final report is not guaranteed. Records already persisted
  remain.
- Dropping the public run future counts as cancellation in every state. The
  run is transferred to the cleanup service, which finalizes and persists it.

## 2. Acceptance cases per OS

Legend:

- **V**: verified by the cited tests in CI on that OS.
- **V (stub)**: verified with an injected stub, as the contract allows for
  that OS.
- **E**: the engine-level logic is verified on that OS with a test executor
  (no real child processes).
- **n/a**: the mechanism does not exist on that OS. The OS's own case replaces
  it.
- **pending**: needs Windows evidence once the Windows supervisor exists (see
  §3).

Test paths are relative to the repository root. `†` marks tests behind the
`storage-test-hooks` feature, which run in the `--all-features` CI step on
every OS. `‡` marks tests compiled only with `debug_assertions` (the CI test
profile). The suites `tests/supervisor_linux.rs`, `tests/job_engine_linux.rs`,
`tests/session_linux.rs` and `tests/cli_signals.rs` are compiled for Linux and
macOS despite their names. Individual tests inside them may be limited to one
OS, as the columns show.

### 2.1 Supervisor acceptance suite

| # | Case | Mechanism | Tests | Linux | macOS | Windows |
|---|---|---|---|---|---|---|
| S1 | Root exits while a descendant holds stdout | post-exit group kill, drain to EOF | `tests/supervisor_linux.rs::pipes_drain_concurrently_and_descendant_writers_close`; `tests/pipes.rs::eof_waits_for_descendant_writer_after_root_exit` | V | V | pending (pipe-layer test runs) |
| S2 | Same, descendant holds stderr | same | same tests (stderr iteration) | V | V | pending |
| S3 | Same, descendant holds neither, and is still killed | post-exit `killpg` + verification | `tests/supervisor_linux.rs::post_exit_kill_removes_live_group_members`, `::deadline_removes_three_descendants_and_confirms_the_group_empty` | V | V | pending |
| S4 | Escaped (`setsid`) descendant holds the pipe | drain deadline 500 ms, `NoEofBeforeDrainDeadline`, outside the cleanup claim | `tests/supervisor_linux.rs::capture_drain_deadline_stops_an_escaped_pipe_holder`, `::escaped_descendant_is_outside_the_cleanup_claim_and_is_killed_by_the_test` | V | V | n/a (Job Objects) |
| S5 | Readers and runtime shut down within bounds | transfer cutoff, outcome delivered by the cleanup service | `tests/supervisor_linux.rs::capture_owner_and_runtime_can_drop_before_any_reader_poll`, `::dropping_capture_wait_freezes_at_transfer_cutoff_without_releasing_a_stalled_worker`, `::transfer_cutoff_starts_at_transfer_after_an_earlier_drain` | V | V | pending |
| S6 | Partial setup failure: pipe creation | injected `EMFILE` on the k-th pipe, partial setup released | `tests/supervisor_linux.rs::pipe_creation_failures_release_partial_setup_without_starting_children` †, `::session_pipe_creation_failures_release_cross_wiring_and_both_admissions` †; `tests/pipes.rs::injected_emfile_fails_only_the_kth_pipe_and_scope_restores_creation` † | V | V | pending |
| S7 | Partial setup failure: process creation | `CreateFailed{spawn, errno}`, no children, fds or slots left | `tests/supervisor_linux.rs::failed_creation_and_stdin_setup_leave_no_children_or_pipe_fds`, `::capture_worker_admission_and_setup_failures_return_all_reservations`, `::failed_capture_setup_transfers_a_sink_stalled_on_close` | V | V | pending |
| S8 | Partial setup failure: limit installation | phase pipe from `pre_exec` (Linux). Limits refused before setup (macOS). | `tests/supervisor_linux.rs::phase_pipe_distinguishes_limit_installation_from_exec_failure`, `::invalid_limits_fail_before_setup_without_creating_children` (Linux); `::macos_rejects_both_limit_kinds_before_workers_files_or_spawn` (macOS) | V | V (refusal) | pending (job commit limit) |
| S9 | Partial setup failure: Job Object | — | none | n/a | n/a | pending |
| S10 | Run inside an existing Job Object (compatible, and one that forbids nesting) | — | none | n/a | n/a | pending |
| S11 | Disk full in the capture sink | `WriteError(ENOSPC)`, child never blocked, budget loss kept separate | `tests/supervisor_linux.rs::real_enospc_reports_write_loss_without_blocking_or_conflating_budget_loss` (Linux, real 1 MiB tmpfs); `::sink_partial_writes_and_eintr_are_retried_and_failures_never_block_the_child`, `::budget_discard_does_not_mask_write_loss_and_events_stop_exactly_at_cap` (failing stub) | V | V (stub) | pending |
| S12 | Record or report persistence failure, disk full | `PersistError::Io(ENOSPC)`, verdict, references and delivery kept | `tests/job_engine_linux.rs::real_enospc_record_and_report_failures_keep_verdict_refs_and_delivery` † (Linux, real tmpfs); `src/jobs/engine/tests.rs::stalled_persistence_bounds_w_times_out_the_queue_and_omits_terminal_path`, `::stalled_report_publication_bounds_s_and_keeps_w_warnings_frozen`, `::report_collision_fails_publication_without_rewriting_or_changing_verdict` | V | E | E |
| S13 | Repeated-run leak check: 50 warm-up, then 5 × 100 | fds, threads, slots, credits, zombies and owned-resource counter equal at every batch boundary | `tests/supervisor_linux.rs::mixed_runs_leave_stable_fds_threads_slots_and_no_zombies`, `::four_hundred_runs_leave_no_zombies_or_descriptors`, `::captured_runs_release_descriptors_threads_and_credits`; `tests/pipes.rs::no_fd_leak_after_100_create_use_drop_cycles` (Linux) | V | V (threads via `PROC_PIDTASKINFO`) | pending (handle counts) |
| S14 | Real Ctrl-C during build, test and custom commands | see §2.4 | see §2.4 | V | V | pending |

The S11 and S12 real-ENOSPC tests mount a 1 MiB tmpfs through root or
`sudo -n`, which GitHub-hosted Linux runners provide. Where neither is
available, they print `SKIP real ENOSPC` and pass without asserting. The stub
tests still run.

### 2.2 Required cases (§10.3)

| # | Case | Mechanism | Tests | Linux | macOS | Windows |
|---|---|---|---|---|---|---|
| R1 | Immediate exit before, during and after observer registration | exit seen once and reaped once. pidfd valid for an unreaped child. kqueue `ESRCH`. | `tests/supervisor_linux.rs::immediate_exit_remains_observable_and_unreaped_until_wait`, `::exit_during_pidfd_registration_is_observed_and_reaped_once` † (Linux, paused registration); `src/supervisor/root_macos.rs::exit_before_registration_takes_esrch_path_and_retains_status` (macOS) | V | V | pending |
| R2 | Forced observer failure → polling | Linux: injected `pidfd_open` `ENOSYS` (note) and `EMFILE` (`Register`). macOS: injected kqueue registration error. | `tests/supervisor_linux.rs::pidfd_open_enosys_and_emfile_fall_back_to_polling` † (Linux), `::exits_and_signals_with_both_observers`; `src/supervisor/root_macos.rs::registration_error_falls_back_and_finite_zero_reaps_back_off` (macOS) | V | V | n/a |
| R3 | `waitid`/`wait4` errors incl. injected `ECHILD` | §3.1.6: stop pid use, `Unknown`, transfer keeps the partial outcome | `src/supervisor/root_unix.rs::wait_errors_stop_pid_use_and_transfer_preserves_partial_outcome` | V | V | n/a |
| R4 | Worker pre-creation failure → `NotStarted{Worker}` | credit exhaustion and injected thread-start failure. Every reservation returned. | `tests/supervisor_linux.rs::capture_worker_admission_and_setup_failures_return_all_reservations`, `::creation_credit_failure_refuses_the_job_before_admission` † | V | V | pending |
| R5 | Limit failure in `pre_exec` (phase-tagged) vs other spawn failure (generic) | phase pipe | `tests/supervisor_linux.rs::phase_pipe_distinguishes_limit_installation_from_exec_failure` | V | n/a (no `pre_exec`, limits refused) | n/a |
| R6 | kqueue success vs error receipt | `EV_RECEIPT` validation | `src/supervisor/sys/macos.rs::receipts_are_acknowledgements_and_exit_events_are_strictly_validated` | n/a | V | n/a |
| R7 | `WAIT_FAILED` (injected) | — | none | n/a | n/a | pending |
| R8 | Cancellation before commit, during OS creation, after commit | commit/cancel atomic on the creation thread | `tests/supervisor_linux.rs::cancel_before_commit_returns_while_creation_is_blocked` †, `::cancel_inside_os_creation_finishes_spawn_then_kills_and_reaps` †, `::cancel_after_commit_finishes_creation_then_kills` †, `::cancellation_is_persistent_and_mid_run_cancellation_kills`; `tests/job_engine_linux.rs::cancel_during_executor_creation_keeps_the_runtime_responsive` †, `::cancel_before_run_mkdir_keeps_the_runtime_responsive` †, `::cancel_before_recipe_completion_keeps_the_runtime_responsive` †; `src/jobs/engine/tests.rs::cancellation_before_admission_skips_without_allocating`, `::cancellation_after_admission_produces_a_not_started_record` | V | V | E (engine only) |
| R9 | Cancellation and force exit during a blocked creation (injected) | cancellation returns without waiting. Force exit kills registered runs and does not wait for the blocked creation. | `tests/supervisor_linux.rs::cancel_before_commit_returns_while_creation_is_blocked` †, `::force_exit_registry_primitive_does_not_wait_for_blocked_creation` † (before and after commit), `::session_cancel_and_drop_between_root_commits_release_both_roots` † | V | V | pending |
| R10 | Drop of the public future in every §3.2 state | transfer to the cleanup service | `tests/supervisor_linux.rs::drop_before_commit_never_creates_a_child` †, `::drop_after_commit_kills_and_transfers_the_created_child` †, `::dropped_owner_is_killed_and_reaped`, `::cancelled_wait_future_transfers_ownership`, `::dropped_exit_transfers_full_outcome_and_cloneable_ticket`, `::cancelled_wait_ticket_records_sigkill_even_before_first_poll`, `::dropping_a_terminating_future_preserves_the_trigger_and_notes_cancellation`, `::dropping_a_paused_wait_reconciles_an_already_exited_root`, `::dropping_capture_wait_freezes_at_transfer_cutoff_without_releasing_a_stalled_worker` | V | V | pending |
| R11 | Root exit with descendants holding stdout / stderr / neither | = S1–S3 | = S1–S3 | V | V | pending |
| R12 | Late `OutputBudget` after root exit is still a violation | budget counts observed bytes until EOF | `tests/supervisor_linux.rs::capture_late_budget_breach_after_root_exit_is_still_a_violation` | V | V | pending |
| R13 | Timer and exit ready in one wakeup, both orders (injected delay) | exit check before enforcement | `tests/supervisor_linux.rs::timer_and_exit_ready_together_are_reconciled_in_both_selection_orders`; `src/supervisor/root_unix.rs::observed_wall_judgment_has_exact_grace_boundaries_and_keeps_enforcement` | V | V | pending |
| R14 | Pending overlapped read during cancellation; completion racing `CancelIoEx` | — | none | n/a | n/a | pending |
| R15 | `ConnectNamedPipe` pending and failing (injected) | — | none | n/a | n/a | pending |
| R16 | Permanently stalled sink keeps its reservation until the writer returns | freeze at 5 s, `WriteStalled`, slot and credit held | `tests/supervisor_linux.rs::stalled_sink_freezes_at_five_seconds_and_keeps_its_slot_and_credit`, `::full_capture_queue_does_not_delay_deadline` | V (stub) | V (stub) | pending |
| R17 | Sink write error (tiny tmpfs on Linux, failing stub elsewhere) | = S11 | = S11 | V | V (stub) | pending |
| R18 | Report persistence stalled and failing | = S12 | = S12 | V | E | E |
| R19 | Event consumer stalled for 10 000 runs; terminal ordering by `seq` | bounded transport, drops counted, `JobFinished` after channel close | `tests/job_engine_linux.rs::ten_thousand_real_runs_with_stalled_events_keep_terminal_sequence_and_report` † (10 000 real runs, consumer never polled); `src/jobs/engine/tests.rs::bounded_events_sequence_gap_equation_terminal_barrier_and_output_opt_in`; `tests/supervisor_linux.rs::capture_flood_is_exact_and_unread_events_stay_bounded` | V | V | E |
| R20 | Cleanup capacity and thread-credit exhaustion (5th admission refused) | 4 slots, 32 credits | `tests/supervisor_linux.rs::four_slots_bound_admission_and_release_after_reap`, `::init_is_idempotent_and_thread_credits_are_bounded_and_returned` | V | V | pending |
| R21 | Transferred run: logical record within its bound while physical cleanup continues | cutoff at transfer + drain deadline | `tests/supervisor_linux.rs::reap_timeouts_transfer_identity_and_ticket_gets_the_physical_outcome`, `::transferred_finished_capture_waits_for_root_facts_or_cutoff`, `::transferred_run_without_captures_publishes_at_cutoff`; `src/supervisor/root_macos.rs::transferred_slow_scan_yields_so_another_run_can_finish` | V | V | pending |
| R22 | Enumeration error, truncation, short `proc_pidinfo`, zombie members | §4.4 scanners | Linux: `src/supervisor/sys/linux_proc.rs::malformed_truncated_and_unreadable_entries_fail_the_scan`, `::completed_scan_uses_fdinfo_numbering_excludes_root_zombies_and_escaped_members`. macOS: `src/supervisor/sys/macos_proc.rs::full_buffers_grow_without_reallocation_and_stop_at_cap`, `::info_requires_exact_size_and_clean_errno_except_for_disappearance`, `::malformed_counts_errors_and_empty_success_are_distinct`; `src/supervisor/root_macos.rs::zombie_lookup_includes_an_unreaped_descendant_but_does_not_count_it_live`, `::libproc_failures_are_unverified_and_keep_measurements_partial` | V | V | pending (job accounting query) |
| R23 | `/proc` prerequisite failures and `hidepid` (injected) → `Unverified` | mountinfo and fdinfo checks | `src/supervisor/sys/linux_proc.rs::prerequisites_never_confirm_an_unsupported_view`, `::stacked_mounts_validate_the_visible_mount_id_and_filtering`; `tests/supervisor_linux.rs::unverified_cleanup_marks_reaped_measurements_partial` | V | n/a | n/a |
| R24 | Zero-byte read completion and `ERROR_BROKEN_PIPE` as EOF | — | none at supervisor level | n/a | n/a | pending |
| R25 | Late persistence rename after `PersistError` | late rename never fills the success cell or changes the report | `src/jobs/persist.rs::late_rename_keeps_timeout_and_never_fills_the_success_cell` †; `src/jobs/engine/tests.rs::late_persistence_rename_keeps_report_final_at_w_without_duplicate_warnings` † | V | V | E |
| R26 | Budget breach plus an independent write error, both reported | losses judged per kind | `tests/supervisor_linux.rs::budget_discard_does_not_mask_write_loss_and_events_stop_exactly_at_cap`, `::real_enospc_reports_write_loss_without_blocking_or_conflating_budget_loss` (Linux); `src/report/tests.rs::supervisor_adapter_fixtures`, `::classify_run_fixtures` | V | V | E (derivation) |
| R27 | `ACTIVE_PROCESS_ZERO` never delivered, still `Confirmed` by polling | — | none | n/a | n/a | pending |
| R28 | Address-space limit with a lower inherited hard limit | `Note::LimitLowered` | `tests/supervisor_linux.rs::inherited_address_space_hard_limit_is_respected` | V | n/a (refused) | n/a |
| R29 | `Connected` variants and `deadline_group` rejected where unsupported | `NotStarted{Unsupported}` | `tests/session_linux.rs::connected_plain_runs_are_unsupported` | V | V | pending |
| R30 | Real Ctrl-C and second Ctrl-C | see §2.4 | see §2.4 | V | V | pending |

### 2.3 Bounds and invariants (§10.1, §10.2)

| # | Case | Tests | Linux | macOS | Windows |
|---|---|---|---|---|---|
| T1 | timer → kill ≤ 50 ms; kill → root exit ≤ 250 ms | `tests/supervisor_linux.rs::deadline_kills_hold_within_the_policy_bounds` | V | V (×4) | pending |
| T2 | root exit → cleanup result ≤ 1 s + 50 ms, also with a scan delayed past the deadline | `tests/supervisor_linux.rs::root_exit_to_cleanup_result_meets_the_cleanup_deadline` †; scanner semantics: `src/supervisor/sys/linux_proc.rs::scan_cut_by_cleanup_deadline_uses_only_the_last_completed_live_count`, `::slow_scan_distinguishes_the_reap_deadline_from_the_cleanup_deadline`; `src/supervisor/sys/macos_proc.rs::newer_scan_cut_by_the_cleanup_deadline_keeps_the_last_live_count`, `::completed_live_scan_at_deadline_differs_from_no_completed_scan` | V | V (×4) | pending |
| T3 | Healthy per-milestone bounds: cleanup → drain 50 ms; drain → readers stopped, → frozen, → finalized, → persisted, → released 100 ms each | `tests/job_engine_linux.rs::healthy_run_milestones_meet_each_bound_over_five_runs` † (real engine, real persistence); end-to-end: `tests/supervisor_linux.rs::short_sleep_exits_without_enforcement_and_records_observed_wall` | V | V (×4) | pending |
| T4 | Consuming-wait retries bounded by the reap deadline, then transfer | `tests/supervisor_linux.rs::reap_timeouts_transfer_identity_and_ticket_gets_the_physical_outcome`, `::slow_cleanup_transfers_at_the_original_kill_reap_deadline` (Linux), `::post_exit_deadline_and_cancellation_do_not_add_late_trigger_notes`; `src/supervisor/root_macos.rs::zero_wait4_after_note_exit_retries_without_another_notification` | V | V | n/a |
| T5 | Escaped pipe: drain 500 ms | = S4 | V | V | n/a |
| T6 | Stalled sink: finalisation 5 s | = R16 | V | V | pending |
| T7 | Reap-timeout path: record ≤ transfer + 500 ms | `tests/supervisor_linux.rs::transferred_run_without_captures_publishes_at_cutoff`, `::transferred_finished_capture_waits_for_root_facts_or_cutoff` | V | V | pending |
| T8 | Late-trigger and cancellation timing notes | `tests/supervisor_linux.rs::cancellation_after_observable_exit_is_only_a_note`, `::deadline_before_observation_after_cancellation_is_a_late_trigger`, `::cancellation_after_a_deadline_kill_and_exit_observation_is_not_a_late_trigger` | V | V | pending |
| I1 | Fixture readiness `READY <pid> <start-id>`, blocking acknowledgement, 5 s timeout kills the group | `crates/cpf-fixture/tests/fixture.rs::ready_pins_tree_identities_and_group_kill_removes_them`, `::nested_spawn_and_repeated_ready_do_not_steal_acks`, `::unacknowledged_ready_times_out_before_executing_next_step`, `::missing_ready_kills_containment_and_fails_the_test` | V | V | V (fixture only) |
| I2 | "Gone" = no live identity (`ESRCH`, other start-id, or zombie) | `crates/cpf-fixture/tests/fixture.rs::killed_root_is_gone_before_it_is_reaped`; identity checks throughout `tests/supervisor_linux.rs` | V | V | pending |
| I3 | Leak checks | = S13 | V | V | pending |
| I4 | Memory: peak RSS in [N, N + 16 MiB + baseline], bytes | `tests/supervisor_linux.rs::wait4_reports_resident_memory_in_bytes`; `crates/cpf-fixture/tests/fixture.rs::touched_memory_stays_alive_until_killed` | V | V | pending (working set, job commit) |
| I5 | CPU time in µs, root + waited descendants | `tests/supervisor_linux.rs::busy_fixture_reports_cpu_microseconds`, `::measurements_select_deduplicate_and_report_unsupported_metrics`, `::unreaped_measurements_remain_absent_and_frozen_after_late_reap` | V | V | pending |
| I6 | Cap boundaries 4095/4096/4097, both streams, all sink and exceed policies | `tests/supervisor_linux.rs::capture_budget_boundaries_clip_both_streams_for_every_policy` | V | V | pending |
| I7 | Flood: 64 MiB on both streams concurrently | `tests/supervisor_linux.rs::capture_flood_is_exact_and_unread_events_stay_bounded`; `crates/cpf-fixture/tests/fixture.rs::flood_drains_both_streams_concurrently` | V | V | V (fixture only) |
| I8 | Start-up: SIGCHLD default, no `SA_NOCLDWAIT`, inherited fds CLOEXEC | `src/supervisor/sys/linux.rs::init_preserves_sigchld_default_and_marks_inherited_fd`; `src/supervisor/sys/macos.rs::startup_sanitizes_inherited_descriptors`; `tests/supervisor_linux.rs::inherited_sigchld_ignore_is_reset_before_spawn` | V | V | pending |
| I9 | Exit codes and signals through both observers; cwd, complete env, file stdin applied | `tests/supervisor_linux.rs::exits_and_signals_with_both_observers`, `::creation_applies_cwd_complete_env_and_file_stdin` | V | V | pending |

### 2.4 Signals, cancellation and force exit (§8.2)

| # | Case | Tests | Linux | macOS | Windows |
|---|---|---|---|---|---|
| C1 | Real SIGINT/SIGTERM during build, test, run, cmd, repeat, `for_each_test`, hooks: cancelled, persisted and delivered before re-raise; descendants gone ≤ 2 s | `tests/cli_signals.rs::signals_during_every_job_persist_and_deliver_before_reraise` | V | V | pending (console Ctrl-C/close) |
| C2 | Second signal forces exit after killing every live job tree | `tests/cli_signals.rs::second_sigint_kills_every_live_job_tree_before_exiting` †; `tests/cli_run.rs::second_sigint_forces_exit_during_console_drain`; `src/jobs/engine/cancel.rs::force_exit_does_not_wait_for_the_verdict_lock` | V | V | pending |
| C3 | Signals during planning refuse without admitting a job | `tests/cli_signals.rs::planning_signals_refuse_without_admitting_a_job` † | V | V | pending |
| C4 | Signals between iterations skip the remaining admissions and keep completed runs | `tests/cli_signals.rs::signals_between_repeat_and_test_iterations_skip_all_remaining_admissions` †, `::signals_in_later_iterations_preserve_completed_runs_and_skip_the_tail`; `tests/cli_cmd.rs::sigint_between_iterations_delivers_cancelled_coverage_before_signal_death` | V | V | pending |
| C5 | Signals after delivery leave the frozen report; read-only commands keep default actions; listener restored | `tests/cli_signals.rs::signals_after_delivery_leave_the_frozen_report_and_status_unchanged_unless_forced` †, `::read_only_commands_and_switch_before_hooks_retain_default_signal_actions` †, `::listener_drop_restores_defaults_and_reinstallation_has_no_pending_intent` | V | V | pending |
| C6 | Delivery failure outranks signal cancellation | `tests/cli_signals.rs::delivery_failure_outranks_real_signal_cancellation_for_every_machine_job` | V | V | pending |

The signal tests send signals to the `cpf` pid, not to a terminal foreground
group. Children run in their own process groups, so this is the same input for
`cpf`.

### 2.5 Connected runs

Connected runs (relay and direct modes, §9a and
[design-checkers](../design-checkers.md) §11) use the same Unix root
coordination. `tests/session_linux.rs` runs on Linux and macOS. It covers
these cases, by fixture: 1 (`fixture_01_wall_limit_and_provenance`), 2
(`fixture_02_sink_gone_reaches_writer_as_sigpipe`), 3–8 (`fixture_03_…` to
`fixture_08_…`), 9–11 (`fixture_09_…` ‡, `fixture_10_…` ‡, `fixture_11_…` ‡),
12–15 (`fixture_12_…` to `fixture_15_…`), 17
(`fixture_17_ten_thousand_exchange_latency_ci`) and 18
(`fixture_18_fifty_warmup_then_five_hundred_sessions_release_all_resources`).
It also covers relay/direct echo, cross-direction independence, the idle
watchdog, atomic pair admission and rollback, transcripts and path safety, and
dialogue caps and recovery records. Fixture 16 (same-wakeup selection) is
platform-independent record logic in `src/supervisor/session/record.rs`
(`selection_rechecks_prerequisites_and_coalesces_stops`,
`exit_precedes_timer_and_cleanup_becomes_eligible_immediately`) and runs on
all three OSes. Status: **V** on Linux and macOS. On Windows, connected runs
return `Unsupported` before admission, so they are **pending**, including
fixture 2's `ERROR_NO_DATA` variant.

**Latency (fixture 17).** The CI test times 10 000 echo round trips in each
mode with a generous 3 ms/round-trip ceiling, and prints the numbers to the
log. On GitHub-hosted runners in the CI test profile, recorded results are
about **31–79 µs** per round trip in relay mode and **14–27 µs** in direct
mode. An earlier relay target of **≤ 35 µs** per round trip is **not met on
every runner**: relay exceeds it on some. The CI test has no performance
threshold beyond the ceiling. The full 100 000-exchange benchmark
(`fixture_17_tiny_exchange_latency_benchmark`) is `#[ignore]` and run by hand.

### 2.6 Case counts

| OS | V | V with stub or refusal | E | n/a | pending |
|---|---|---|---|---|---|
| Linux | 58 | 1 | 0 | 8 | 0 |
| macOS | 51 | 4 | 2 | 10 | 0 |
| Windows | 2 (fixture harness only) | 0 | 6 | 9 | 50 |

These counts cover the 67 rows of §2.1–§2.4 (S1–S14, R1–R30, T1–T8, I1–I9,
C1–C6). Connected-run fixtures (§2.5) are counted separately. All 18 pass on
Linux and macOS. On Windows, only fixture 16 (record logic) runs, and the
other 17 are pending.

## 3. Windows: current limitation

The Windows supervisor is **not implemented**. Every job command (`cpf build`,
`cpf test`, `cpf run`, `cpf cmd`) refuses **before admission** with class
`infrastructure` and exit status 10. No job directory, run or child is
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

Until then these rows are "pending", not failures. No Windows behaviour is
claimed.

## 4. Residual risks and caveats

- **Unregistered child on force exit (Unix).** Force exit does not wait for a
  creation in progress. A child whose `spawn` is still running, or has
  returned but is not yet in the registry, can survive force exit for an
  unbounded time. There is at most one per job, because each job has one
  creation thread (§3.4, §11). R9 proves that force exit does not wait and
  that registered runs are killed. Its force-exit half drives the registry
  primitive directly. The end-to-end second-signal test (C2) does not
  combine force exit with a blocked creation.
- **Escapes and third-party reaping.** Cleanup verification observes the
  process group. Descendants that call `setsid`/`setpgid`, or that are reaped
  by someone else, are outside the claim (S4). This is not a sandbox.
- **macOS cleanup verification.** It relies on libproc
  (`proc_listpids(PROC_PGRP_ONLY)`, `proc_pidinfo(PROC_PIDTBSDINFO, 1)`).
  `PROC_PGRP_ONLY` is supplied locally as SDK constant 2, because the pinned
  `libc` omits it. A scan is not an atomic snapshot. The kernel calls cannot
  be pre-empted, so deadlines are checked around them, not during them. Groups
  over 64 Ki pids are `Truncated`. It is validated only on the macOS release
  and architecture that CI runs (arm64, macOS 26). Elsewhere a failing probe
  yields `Unverified{Unsupported}`, which does not block a pass. Peak RSS on
  Darwin is the `wait4` high-water mark in bytes, not a simultaneous tree peak.
- **Partial-forward test uses a barrier.** Fixture 11 checks the exact
  4096-byte forwarded prefix with a debug-only relay pause
  (`relay_pause_after`), because Darwin cannot resize pipes. A real
  kernel short write in the relay is not produced. The bytes and bounds
  asserted are the same, but the trigger is synthetic on both OSes.
- **Real ENOSPC needs a tmpfs mount.** S11 and S12 use a real disk-full
  condition only on Linux hosts with root or passwordless `sudo`. Elsewhere
  the real-disk half is skipped with a message, and only stubs run. On macOS
  the sink error is always a stub.
- **Debug-build stack depth.** In debug builds, the poll frames of the job
  state machine exceed the 2 MiB default stack of test threads. The CLI drives
  the job on the main thread (8 MiB by default on Linux and macOS). The
  real-engine tests do the same on an explicit 8 MiB thread
  (`tests/job_engine_linux.rs`, `tests/ready_jobs_linux.rs`), and the per-run
  execution future is boxed to keep it off the job future's frame. No test
  bounds the frame size itself. Embedding the engine on a smaller stack in a
  debug build would overflow.
- **Timing on hosted runners.** Allowances are an assumption about CI hosts
  (×4 on macOS). A supervisor stall longer than the 50 ms grace (heavy load,
  a suspended `cpf`), or polling observation (≤ 10 ms), can produce a
  deadline violation for a program that finished in time. `Timing.observer`
  and the wall method record how exit was observed. Some load-sensitive tests
  (the cleanup-capacity test and the fixture readiness handshake on macOS)
  have failed intermittently on hosted runners in the past. Those failures
  are treated as harness issues. Bounds are not relaxed for them.
- **Relay latency** exceeds the earlier ≤ 35 µs target on some runners
  (§2.5).
- **Unverified cleanup passes.** A pass does not imply verified cleanup. The
  summary says "cleanup not verified" when it is not, and measurements stay
  `Partial` unless cleanup is `Confirmed`.
- **Raw `cpf run` exit.** At exit, physical cleanup still pending (a stalled
  writer, an unreaped root after `ReapTimeout`) is left to process teardown.
  An unreaped root is reparented to init, and the record says so (§8.1).
