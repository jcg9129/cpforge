# Execution contract

Status: v5. Normative for the `supervisor`, `jobs` and `report` modules.
Storage (run directories, leases, pruning, retention accounting) is defined
in `storage.md`; configuration (how a spec is produced, placeholders, shell
mode, hooks, `repeat`, runs per job) is defined in `config.md`; result
derivation, the report format, exit statuses and machine output are
defined in `report.md` (the report contract).

MUST / MUST NOT / never are requirements. Defaults are overridable by
configuration (see `config.md`) unless marked *fixed*. Where a guarantee is
narrower than
one might expect, the narrowing is stated in place; this contract prefers an
honest weak guarantee to an unproven strong one.

## 1. Terms and guarantee levels

- **Run:** one supervised process tree started from one
  `ResolvedExecutionSpec` (a build, one test execution, one `repeat`
  iteration, one custom command or hook invocation).
- **Job:** a cancellable unit made of runs (`cpf test` = build + one run per
  test + comparisons). Jobs own cancellation and produce the final report.
- **Root:** the process `cpf` creates. **Containment unit:** Unix process
  group, Windows Job Object. **Tree:** the live members of the unit.
- **Supervisor:** the per-run state machine. **Cleanup service:** one
  process-wide component owning resources that outlive their supervisor
  (§3.3).

Facts kept separate, never merged: **termination requested**, **root
exited**, **cleanup result**, and four milestones:

1. **capture frozen:** stream outcomes (bytes stored, completeness, losses)
   are fixed; later writes, if any, are not counted and the file is not used;
2. **record finalized:** the `RunRecord` exists in memory and never changes;
3. **record persisted:** written to the run directory (or a `PersistError`);
4. **resources released:** every pipe, handle, thread, reservation and
   process identity of the run is gone.

Unix process-group escape (`setsid`, daemonising) and processes reaped by
someone else are documented limitations; this is not a sandbox.

## 2. Input type

```rust
pub struct ResolvedExecutionSpec {
    pub stage: Stage,                    // Build | Run | Command | Hook (Checker, Generator, Brute,
                                         // Interactor: later features)
    pub program: PathBuf,                // resolved executable; shell mode already turned into program+args (configuration contract)
    pub args: Vec<OsString>,
    pub cwd: PathBuf,                    // absolute (storage contract decides which directory)
    pub env: EnvSpec,                    // complete final environment (configuration contract)
    pub stdin: StdinSource,              // Null | File(PathBuf) | Forward (raw mode, §8.1)
                                         // | Connected(PeerRef) (reserved, §9a)
    pub stdout: StreamPolicy,
    pub stderr: StreamPolicy,
    pub wall_deadline: Duration,         // always present, > 0
    pub deadline_group: Option<GroupRef>,// reserved, §9a; CLI foundation: always None
    pub limits: Vec<LocalLimit>,         // validated and normalised before spawn (§7.2)
    pub measure: Vec<MetricKind>,
}

pub struct StreamPolicy {
    pub sink: Sink,                      // File(PathBuf) | Forward(ConsoleStream) | Discard
                                         // | Connected(PeerRef) (reserved, §9a)
    pub budget: u64,                     // output budget on observed bytes, independent of sink
    pub on_exceed: Exceed,               // Kill | Discard
    pub events: bool,                    // emit OutputChunk events (TUI, tests); default false
}
```

The spec is immutable. The supervisor never reads configuration or workspace
files. Declared artifacts (design §5.2) are validated by build orchestration
(build planning), not by the supervisor. Every run persists its resolved spec as a
versioned record (location: storage contract); `spec_digest` is SHA-256 over a canonical
encoding: Unix `OsString` as raw bytes, Windows as WTF-16 code units,
length-prefixed, fields in declaration order. `spec.json` encodes native
values as report contract §3.3 `ns`; `spec_digest` is unaffected (it is
computed over this binary encoding). Binary framing uses little-endian
`u64` integers and vector counts, byte-length-prefixed native strings and
enum tokens, and one byte (`0` or `1`) for booleans. Windows native strings
contain little-endian WTF-16 code units; their length prefix counts bytes.
Enum tags use the schema's lowercase tokens; durations use microseconds,
and an absent deadline group uses the token `none`. If `spec.json` has
already been written with another native encoding, this is a `spec.json`
schema increment. The serialised spec has a size
limit (configuration contract: 256 KiB) checked before the run is admitted. In the CLI foundation,
`Connected` variants and a `deadline_group` are rejected at validation
(`NotStarted{Unsupported}`).

## 3. Ownership

### 3.1 Rules

1. Exactly one owner at a time holds each run's resources: the root's
   identity (pid / pidfd / kqueue registration / process handle), the
   containment unit, every pipe end, every sink, every worker thread, every
   storage reservation. Ownership moves only by explicit transfer, exactly
   once. Transfer is non-blocking and allocation-free (a pre-reserved slot,
   §3.3).
2. Only `supervisor::sys` may wait on a child. Waiting on `-1` or on a process
   group is forbidden program-wide. `SIGCHLD` keeps `SIG_DFL` and
   `SA_NOCLDWAIT` is never set.
3. `tokio::process`, `std::process::Child::{wait, try_wait, kill}` and crates
   that spawn or reap children are not used; Tokio's `process` feature is off
   and CI checks it. On Unix, `std::process::Command::spawn` is an
   **authorised creation transaction**: if it returns `Err`, `std` has already
   reaped any child it forked; ownership begins at `Ok`. The returned
   `Child` is decomposed into pid and pipe ends and forgotten. Re-audited when
   MSRV or the toolchain changes.
4. `NotStarted` means "the target program did not start executing", not "no
   OS process ever existed".
5. **No unreaped root is ever released.** A finalized or persisted record
   does not end ownership of the root; only a successful consuming wait
   (Unix) or closing the process handle after it is signalled (Windows) does.
6. **Signalling capability**: numeric `killpg(pgid)` is allowed only
   while the root is known unreaped. The supervisor clears the capability
   flag, under the live-run registry lock, **before** the consuming `wait4`.
   Any wait error that means identity protection may be lost (`ECHILD`, or
   any error other than `EINTR` from a consuming wait) clears it at once,
   sets status `Unknown` and cleanup `Unverified{ObserverFailed}`.
   Windows: `TerminateJobObject` is allowed while the run owns the job
   handle, regardless of root state. Force exit uses only this capability,
   never the state name.

### 3.2 Supervisor states

```
Prepared → Creating → Running → Terminating → RootObserved → Reaped → Frozen → Finalized
                 \                                                        \
                  → NotStarted                     any state ≥ Running ───→ Transferred
```

| State | Owns | Drop of the public future does |
|---|---|---|
| Prepared | pipes, sinks, reservations | release them; `NotStarted{CancelledBeforeStart}` |
| Creating | creation in progress (§3.4) | publish cancel; the creation thread finishes, then the kill path runs |
| Running | everything | request termination, transfer |
| Terminating | everything, kill sent | transfer |
| RootObserved | root unreaped (Unix) / handle (Windows); signalling capability set | transfer |
| Reaped | pipes, sinks, workers, job handle (Windows); signalling capability **cleared** | transfer |
| Frozen | capture outcome fixed; possibly a stalled writer and its reservation | transfer |
| Finalized | record in memory; nothing else unless something was transferred | nothing |

Dropping the public future always counts as cancellation of the run; the
cleanup service finalizes the outcome in memory, publishes it through the
transfer ticket and releases resources. With no live engine to persist it,
the record is best effort: cleanup owns no persistence queue. `cpf fsck`
reports the schema of each run file present (`valid schema 1`); it does not
diagnose an absent `record.json`.

### 3.3 Cleanup service and global bounds

- One process-wide service owns transferred resources. Its single
  **polling thread** is created when `cpf` starts (start-up fails cleanly
  otherwise) and never blocks: it observes and reaps transferred roots by
  polling with backoff (Unix `waitid(P_PID, pid, WEXITED | WNOHANG |
  WNOWAIT)`, then `wait4(pid, WNOHANG)`; Windows `WaitForSingleObject(process,
  0)`) and receives completions from transferred workers. It is also the
  recovery route for every observer failure; the cleanup path never needs a
  new thread.
- Bounds (*fixed*): **4** cleanup slots and **32** credited supervisor worker
  threads (cleanup, creation, persistence, sinks, forwarders and transcript
  writers). The fixed process/command threads `cpf-control`, `cpf-stdout`
  and `cpf-stderr` (at most one each) are outside the credits: the process-wide
  maximum is 32 credited threads plus these three. Every
  admitted run holds a slot from admission to resources released, so at most
  **4 runs are admitted concurrently** (the CLI foundation runs tests sequentially; an
  interactive pair needs 2).
- **Workers, all created before any child exists**: per job, one
  creation thread and one persistence thread, created at job start after
  reserving their 2 thread credits atomically from the 32 (failure: credits returned, the
  job does not start, nothing to recover). Job threads hold no cleanup slot;
  their credits return when the job's threads have exited, or pass with a
  stalled persistence thread to the cleanup service (§6.2), which holds it
  without a cleanup slot until the thread has exited. Per run, at admission: the
  Windows waiter, one sink worker per `File`/`Forward` stream, the stdin
  forwarder in raw mode. Unix observers need no thread (pidfd / kqueue, or
  polling, §4.1).
- **Admission:** before entering `Creating`, a run reserves atomically and
  without blocking one cleanup slot (with its transfer-queue entry) and its
  thread credits, then creates its workers. Reservation failure →
  `NotStarted{CleanupCapacity}`; worker creation failure →
  `NotStarted{Worker}`, workers created so far stopped, credits returned.
  Nothing is left reserved.
- Closing a job's creation queue hands its thread handle and credit to the
  cleanup poller without taking a run slot or waiting for setup I/O. The
  poller releases the credit only after joining the finished thread.
- Credits and the slot are returned only at **resources released**. A run
  whose root is never reaped (uninterruptible sleep) or whose writer never
  returns keeps its slot for the life of the `cpf` process; this is reported
  (`ReapTimeout`, `WriteStalled`), and further runs are refused once all
  slots are held.

### 3.4 Creation

- Each job's **creation thread** (§3.3) performs every potentially blocking setup step in order: opening stdin and
  sink files, creating pipes, the job object (Windows), the OS creation call.
  Async code never performs them.
  Run-directory creation, recipe resolution (deferred program completion and
  staging for cwd), and post-build validation also run on the creation thread.
  Cancellation interrupts the first two at step boundaries (the run is
  `NotStarted{CancelledBeforeStart}`, report contract §4.5); post-build
  validation is awaited, since it decides the build result.
- Per run, `committed` and `cancel` are atomics. The creation thread checks
  `cancel`, sets `committed`, calls the OS creation function. Cancellation
  sets `cancel` and returns **without waiting**; if `committed` was set, the
  creation thread registers the child under the registry lock as soon as
  creation returns and then runs the kill path through cleanup transfer.
  Unless creation's reply has already arrived (the run then continues and
  its wait runs the kill path), the caller receives `Transferred`:
  cancellation occurred during committed creation, and whether a child
  exists is unknown at return time. The live
  job persists this record and its report through the ordinary engine path;
  cleanup's later ticket outcome does not replace the finalized record.
  Connected-pair creation follows the same rule for both runs.
- `setup_duration` (first setup step to end of creation) is recorded and is
  **outside** the deadline guarantee. The deadline timer starts when creation
  returns.
- The job does not wait for that kill path either: its orderly exit can come
  first. A child registered at creation return is killed through its
  signalling capability by orderly re-raising and by force exit (§8.2).
- Neither orderly exit nor force exit (§8.2) waits for a creation in
  progress. Narrowed guarantee: a child that is not yet registered (its
  creation call still running, or returned but not yet published in the
  registry) may survive the exit of `cpf` on Unix, for an unbounded time;
  there is at most one per job, since each job has one creation thread. On
  Windows it dies with its job when `cpf` exits (kill-on-close).

### 3.5 Inheritance

All handles and descriptors `cpf` creates are non-inheritable /
close-on-exec, except the child ends of the standard streams (including a
null stdin handle), created per run and handed only to that run. On Unix,
descriptors used to set up the standard streams stay CLOEXEC until `dup2`
onto 0–2 in the child. At start-up `cpf` marks every inherited descriptor
≥ 3 close-on-exec (Linux `close_range(3, ~0, CLOSE_RANGE_CLOEXEC)` on ≥ 5.11,
else iterate `/proc/self/fd`; macOS iterate `/dev/fd`).

## 4. Lifecycle per OS

After every failure the text names who keeps observing and reaping; the
"kill path" always uses an observer that exists. **Exit check** means
a non-blocking check of whichever observer the run actually has: pidfd
`poll` (timeout 0), `kevent` (zero timeout), one polling step, or
`WaitForSingleObject(process, 0)`.

### 4.1 Linux (kernel ≥ 5.4)

1. Admission (§3.3). Creation thread: open stdin source, create
   stdout/stderr pipes (`O_CLOEXEC`, parent ends non-blocking).
2. Commit, spawn with `Command`, `CommandExt::process_group(0)`. If an
   address-space limit is requested, a `pre_exec` closure calls only
   `setrlimit(RLIMIT_AS, …)` (async-signal-safe, no allocation). On failure it
   writes a phase byte and `errno` to a CLOEXEC **phase pipe** created for
   this spawn, then returns the error. When spawn returns `Err`, the creation
   thread reads the pipe without blocking: a phase record →
   `NotStarted{Limit(errno)}`; none → `NotStarted{CreateFailed{api: "spawn",
   raw_error}}`. Inherited soft/hard limits are sampled at admission
   and kept in `InstalledLimit`.
3. Register the child (pid = pgid, signalling capability set). Close child
   ends.
4. Observer: `pidfd_open(pid, 0)` (valid for an exited, unreaped child). On
   **any** error the supervisor **polls** instead, without a thread:
   `waitid(P_PID, pid, WEXITED | WNOHANG | WNOWAIT)` with backoff 1, 2, 4 …
   10 ms (observes exit without reaping; adds ≤ 10 ms to observation,
   recorded in `Timing.observer`). `ENOSYS`/`EPERM` record
   `Note::ObserverFallback`; other errnos `SupervisorError::Register(errno)`.
   `EINTR` is retried; any other `waitid` error applies §3.1.6.
5. Start stream readers (§5). Wait for: exit readiness, deadline timer,
   cancellation, budget-exceeded signal (§6.3 for simultaneous readiness).
6. **Root exited** (RootObserved): `killpg(pgid, SIGKILL)`; `ESRCH` fine.
   Cleanup verification (§4.4). Clear the signalling capability (§3.1.6),
   then `wait4(pid, WNOHANG)` (retry `EINTR`; `0` after readiness is
   `SupervisorError::Reap`, retried up to the reap deadline, then transfer
   with the root still owned; any other error applies §3.1.6) → status +
   rusage. State Reaped.
7. **Deadline / cancellation / budget:** record trigger, `killpg(pgid,
   SIGKILL)`, wait for exit readiness up to the reap deadline, then step 6.
   No readiness → transfer with `ReapTimeout`; the cleanup service's polling
   thread reaps when the root exits.

### 4.2 macOS (13+)

As Linux, with:

- Step 4: one kqueue (CLOEXEC) per run; register `EVFILT_PROC`, `NOTE_EXIT`,
  `EV_ADD | EV_RECEIPT`. With `EV_RECEIPT` every change returns an `EV_ERROR`
  entry: `data == 0` is **success**, otherwise `data` is the errno.
  `ESRCH` means the root already exited: go to step 6. Other errors:
  `SupervisorError::Register` and polling as on Linux.
  `EINTR` is retried. An exit event is accepted only with
  `filter == EVFILT_PROC`, `fflags & NOTE_EXIT`, `ident == pid`.
- Step 6: `wait4(WNOHANG)` returning `0` right after `NOTE_EXIT`/`ESRCH` is
  retried with backoff up to the reap deadline; then transfer, and the cleanup
  service keeps polling (the root stays owned, §3.1.5).
- `RLIMIT_AS` is not offered; requests are rejected before spawn (§7.2).

### 4.3 Windows (10 1809+)

1. Admission. Creation thread: stdout/stderr via `CreateNamedPipeW` (unique
   name, `PIPE_TYPE_BYTE | PIPE_READMODE_BYTE`, parent end
   `FILE_FLAG_OVERLAPPED`, non-inheritable, one instance,
   `PIPE_REJECT_REMOTE_CLIENTS`); child end via `CreateFileW`, inheritable.
   Then `ConnectNamedPipe` on the parent end with an `OVERLAPPED` whose
   manual-reset event (`CreateEventW(NULL, TRUE, FALSE, NULL)`) belongs to
   the pipe: nonzero return or `ERROR_PIPE_CONNECTED` = connected;
   `ERROR_IO_PENDING` → wait up to 1 s, then the completion-ownership rule
   below; anything else is failure (`NotStarted{Setup}`). Unresolved
   connection state is transferred with the run's slot. Stdin: file
   opened inheritable, or `NUL`.
2. `CreateJobObjectW` (unnamed, non-inheritable): `KILL_ON_JOB_CLOSE`,
   `DIE_ON_UNHANDLED_EXCEPTION`, optional `JOB_MEMORY` (§7). Associate a
   completion port (wakeups only, §4.4).
3. Attribute list: `PROC_THREAD_ATTRIBUTE_JOB_LIST = [job]`,
   `PROC_THREAD_ATTRIBUTE_HANDLE_LIST = [child stdin, stdout, stderr]`.
4. Commit, `CreateProcessW(bInheritHandles = TRUE,
   EXTENDED_STARTUPINFO_PRESENT | CREATE_UNICODE_ENVIRONMENT |
   CREATE_NEW_PROCESS_GROUP | CREATE_NO_WINDOW)`, `STARTF_USESTDHANDLES`.
   Failure → `NotStarted{CreateFailed{api, raw_error}}`;
   `ContainmentUnavailable` only when the outer job is identified
   (`IsProcessInJob` + its limits) as the cause. Every handle created
   so far is closed.
5. Register (job handle = signalling capability). Close thread handle, child
   pipe ends, attribute list.
6. **Waiter thread** (reserved): `WaitForMultipleObjects([process,
   control])`, `control` an **auto-reset** event. A control signal
   only wakes it to acknowledge "kill requested"; it keeps waiting for the
   process. It ends when the process is signalled or on `WAIT_FAILED`, in
   both cases after acknowledging to the supervisor; its handles are closed
   only after the acknowledgement. On `WAIT_FAILED`:
   `SupervisorError::Wait`, `TerminateJobObject`, and the cleanup service
   polls `WaitForSingleObject(process, 0)`; the handle stays owned. (The
   waiter exists before creation, §3.3.)
7. **Root exited:** `TerminateJobObject(job, 1)`; cleanup verification
   (§4.4). Then `GetExitCodeProcess`, `GetProcessTimes` (lifetime
   measurement only, §6.3),
   `GetProcessMemoryInfo`, final job accounting (`Partial` unless cleanup is
   `Confirmed`).
8. **Deadline / cancellation / budget:** record trigger, `TerminateJobObject`,
   then as step 7; process not signalled by the reap deadline → transfer.
9. Close the job handle last.

**Completion ownership, for every overlapped operation** (reads and
`ConnectNamedPipe` alike): stop submitting → `CancelIoEx` → wait
for the operation's completion (`GetOverlappedResult`, also for
`ERROR_OPERATION_ABORTED`, or when the cancel returns `ERROR_NOT_FOUND`
because it already completed) → only then release buffer, event and
`OVERLAPPED`. An operation still pending at its deadline is transferred,
never freed early. Job emptiness says nothing about I/O completion.

### 4.4 Cleanup verification

```rust
pub enum Cleanup {
    Confirmed,                                // no live member observed by the stated algorithm
    Incomplete { survivors: u32 },            // live members observed at the deadline
    Unverified { reason: UnverifiedReason },  // unavailable, failed, truncated, observer failed
}
```

`Confirmed` means exactly: **the stated algorithm, run after the kill,
observed no live (non-zombie) member of the containment unit other than the
root.** It is no proof against escapes (§1) and implies nothing about I/O
completion. Each algorithm repeats with backoff (1, 2, 4 … ms) until
`Confirmed` or the cleanup deadline; the result at the deadline is final.

Only a **completed scan** of a **supported view** can confirm; anything else
is `Unverified`. A scan observes current state per entry; it is not
an atomic snapshot.

- **Linux:** prerequisites: `/proc` mounted; **the scan uses procfs's own
  numbering**: the `Pid:` field of `/proc/self/fdinfo/<pidfd>`
  for the root's pidfd is the root's pid in the PID namespace of that procfs
  instance. Since the root is its group's leader, that number is also the
  group's pgid **as procfs reports `pgrp`**, so the scan matches `pgrp` against
  it and excludes the root by it; the pid `spawn` returned is not used for the
  scan, and no namespace equality is assumed (descendants' numbers need not
  match ours). `Pid: 0` (procfs instance cannot see the root) or no pidfd
  (polling fallback) → `Unverified{NamespaceUnknown}`. The Linux cleanup implementation must
  validate this on the supported kernels. The mount options
  in `/proc/self/mountinfo` show no process filtering (`hidepid` absent, `0`
  or `off`); other or unparseable options → `Unverified{ProcFiltered}`. A scan
  iterates every numeric `/proc` entry to the end of the directory and reads
  `/proc/<pid>/stat` into one reused 4 KiB buffer, parsing `state` and `pgrp`
  after the last `)`. Members with `pgrp == pgid`, `pid != root`, state not
  `Z`/`X` are live; entries vanishing during the scan are skipped. A record
  longer than the buffer or unparseable → `Unverified{Malformed}`; an I/O
  error other than `ENOENT`/`ESRCH` → `Unverified{Io}`; a scan cut off by the
  deadline → `Unverified{ScanIncomplete}`.
- **macOS:** `proc_listpids(PROC_PGRP_ONLY, pgid, buf, size)`. The wrapper
  returns 0 on error and for an empty list, so `errno` is cleared before and
  checked after the call; the result is a byte count that MUST be a multiple
  of `sizeof(pid_t)`; a result equal to the buffer size may be truncated →
  retry with a doubled buffer (max 64 Ki pids), else `Unverified{Truncated}`.
  For each pid ≠ root, `proc_pidinfo(PROC_PIDTBSDINFO)` (also 0 on error:
  clear and check `errno`) MUST return exactly `sizeof(struct proc_bsdinfo)`;
  then `pbi_status != SZOMB` decides liveness and `pbi_pgid == pgid`
  membership. `ESRCH` = gone; another error or a short return →
  `Unverified{InfoFailed}`; fields are never read from an unvalidated
  result.
  The macOS cleanup implementation must validate this on each supported
  macOS version; where unusable, the result is `Unverified{Unsupported}`.
- **Windows:** the `ACTIVE_PROCESS_ZERO` port message is only a wakeup
  (delivery not guaranteed). Truth is
  `QueryInformationJobObject(JobObjectBasicAccountingInformation)
  .ActiveProcesses == 0`, polled with backoff and queried once more at the
  deadline. Query failure → `Unverified{QueryFailed(err)}`.

`Unverified` is reported and **does not block a pass**; `Incomplete` does
(§6.4). A pass therefore does not imply verified cleanup, and the summary
says so when cleanup is unverified.

## 5. Streams and capture

### 5.1 Pipeline

`pipe → reader → bounded queue → sink worker → sink`, per stream.

- **Readers** are async tasks on non-blocking / overlapped pipe ends, 64 KiB
  chunks. Native results are normalised first, then the rules below
  apply:

  | Native result | Normalised |
  |---|---|
  | Unix `read` > 0; Windows completion with n > 0 | data |
  | Unix `read` = 0; Windows `ERROR_BROKEN_PIPE` | EOF |
  | Windows success with 0 bytes (a zero-byte write) | nothing; resubmit |
  | Windows `ERROR_OPERATION_ABORTED` after our own `CancelIoEx` | expected stop (no loss of its own) |
  | `EINTR`, `EAGAIN`, spurious readiness; `ERROR_IO_PENDING` | retry / wait |
  | anything else | `CaptureLoss::PipeRead(err)`, stream ends |

  Readers never block on a sink, a forwarder or the event consumer.
- **Budget** counts observed bytes, whatever the sink; `u64` with checked
  arithmetic.
- **Storage cap = budget.** Sink admission stops **exactly** at the budget: a
  chunk crossing it is split, the prefix is queued, the rest is not stored.
  Then `Kill` signals the supervisor and `Discard` keeps reading and dropping
  (`CaptureLoss::BudgetDiscard`); either way readers keep observing until EOF
  or drain expiry, so `observed` may exceed `stored`.
- **Queue** to the sink worker: 1 MiB per stream (*fixed*). When full, the
  reader stops reading (the child back-pressures); deadline and cancellation
  are unaffected.
- **Sink workers** are reserved threads doing blocking writes; partial writes
  continued, `EINTR` retried, a zero-byte write is an error. A write, flush
  or close error → `CaptureLoss::WriteError(errno)`; the stream switches to
  discard; the child is never blocked by our failure.
- Sinks are flushed and closed, not fsynced.

### 5.2 Drain and freeze

- **Drain start** (`t_drain_start`): the root reaped, or the run
  transferred. Readers continue until EOF or the **drain deadline**, then
  **stop**: stopped means no further bytes are admitted into the capture,
  whether or not a pending Windows read has completed (its completion is
  physical cleanup). A stream without EOF records
  `CaptureLoss::NoEofBeforeDrainDeadline`: an observation, not a cause (a
  process outside containment holding the pipe, a back-pressured reader and
  slow teardown look the same). Bytes already read are still queued;
  unread pipe contents are lost under the same entry.
- **Capture frozen** when, per stream, the reader has stopped and either the
  worker has written and closed everything queued, or the **sink finalisation
  deadline** passed. A worker still blocked then records
  `CaptureLoss::WriteStalled`; `stored` is the snapshot of bytes it had
  confirmed written; the file is never used for comparison or reported
  complete; worker, file, lease and storage reservation are transferred and
  released only when the worker returns.
- **Transferred runs**: the cleanup service finalizes the logical
  record at the earlier of "all facts known" and **transfer + drain
  deadline**. Facts still unknown then are recorded as: status `Unknown`,
  cleanup `Unverified{RootNotObserved}` (or the actual reason), capture frozen
  with the losses observed so far. This cutoff **overrides** the sink
  finalisation deadline: any sink with an unfinished operation (queued bytes,
  write, flush or close in progress) records `WriteStalled`, is incomplete,
  is never compared, and keeps its resources until the worker returns.
  Physical cleanup continues; `cleanup.log` remains the initial summary
  (§6.2). A job waits for its runs' logical records, never for resources released.

### 5.3 Capture record

```rust
pub struct StreamCapture {
    pub observed: u64,            // bytes read from the pipe
    pub stored: u64,              // bytes confirmed written to the sink at capture freeze
    pub forwarded: u64,           // bytes written to the console (raw mode)
    pub eof: bool,
    pub exceeded_at: Option<u64>, // observed offset where the budget was first exceeded
    pub losses: Vec<CaptureLoss>, // ≤ 8, deduplicated by kind: BudgetDiscard | WriteError | WriteStalled
                                  // | NoEofBeforeDrainDeadline | PipeRead | ForwardIncomplete
    pub sink: Option<PathBuf>,
}
```

`complete = eof && losses.is_empty() && exceeded_at.is_none()`. For
`Sink::Discard` without a budget breach, discarding is intentional and
records no loss.

### 5.4 Retention reservations

Invariant, **per job**, kept atomically by the storage accounting interface
(the workspace budget is a soft pruning target defined in the storage
contract §6, not a reservation ceiling):

`retained bytes + reserved possible future writes ≤ budget`

- Before admission the job reserves the sum of `budget` over streams with a
  `File` sink (exact, since admission clips at the budget) plus a fixed
  **metadata reservation** of 1 MiB for the persisted spec (≤ 256 KiB),
  record (≤ 64 KiB), `cleanup.log` (≤ 64 KiB) and their temporary
  publication files; held until the persistence thread has returned from the
  last write. Checked arithmetic. Failure →
  `NotStarted{RetentionBudget}`, nothing stays reserved, the job admits no
  further runs. Before its first run the job also reserves the report bound
  of report contract §3.7 for `report.json` and its temporary file.
- At **resources released** the reservation is converted: bytes actually on
  disk become retained, the rest is returned. A stalled writer keeps its full
  stream reservation until it returns (a directory lease alone does not
  preserve headroom).
- Budgets count **logical file lengths**, not allocated blocks.
- Other tool-owned retained classes (copied test inputs and answers, build
  artifacts, job reports) are governed by the storage contract; storage MUST
  reserve them through the same interface before creating them.
- Files a program writes itself are outside any quota guarantee.

### 5.5 Default budgets and deadlines

| Item | Default |
|---|---|
| Run stdout | 64 MiB, `Kill` |
| Run stderr | 1 MiB, `Discard` |
| Build / command / hook stdout and stderr | 1 MiB each, `Discard` |
| Raw `cpf run` | 64 MiB per stream, `Kill` (stdout), `Discard` (stderr) |
| Wall deadline when none configured | build 60 s, run 10 s, command 60 s, hook 30 s |
| Deadline grace (§6.3) | 50 ms (*fixed*) |
| Reap deadline, from kill request (or from root exit observed when no kill preceded it, i.e. the post-exit containment kill) | 2 s (*fixed*) |
| Cleanup verification deadline, from root exit observed | 1 s (*fixed*) |
| Drain deadline, from drain milestone | 500 ms (*fixed*) |
| Sink finalisation deadline, from drain milestone | 5 s (*fixed*) |
| Persistence deadline, from the enqueue call (records: from record finalized; §6.2) | 5 s (*fixed*) |
| Persistence queue credits Q (§6.2) | 2 MiB; charge 8 KiB + encoded length (record: 64 KiB bound) (*fixed*) |
| Persistence credit wait limit, from the enqueue call | 1 s (*fixed*) |
| Persistence credit budget B per job | 10 s + 4 ms × 3 × planned executions (*fixed* formula) |
| Human stderr final-drain deadline, from the final-batch enqueue instant t_q (not from delivery; rendering and lock acquisition before t_q are not covered); absolute, covers every write call, partial write and retry (report contract §8) | 5 s (*fixed*) |
| Console flush deadline (raw mode), from drain milestone | 2 s (*fixed*) |
| Raw final-diagnostic wait (raw mode), from the enqueue instant of the summary line; separate from the console flush deadline (report contract §8) | 2 s (*fixed*) |
| Event transport | 8 MiB total incl. backing buffers and terminal slot; `OutputChunk` ≤ 4 KiB (§9) |
| Retained tool data | 1 GiB per job, 4 GiB per workspace `runs/` (storage contract enforces) |

## 6. Outcome model

### 6.1 Types

```rust
pub enum Execution {
    NotStarted { reason: NotStarted },  // CancelledBeforeStart, CreateFailed{api, raw_error}, Limit(errno),
                                        // UnsupportedLimit, Unsupported, ContainmentUnavailable, Setup(io),
                                        // CleanupCapacity, Worker, RetentionBudget, SpecTooLarge
    Started(Started),
    Transferred,                         // cancelled during committed creation; child existence unknown
}
pub struct Started {
    pub status: Termination,
    pub stop_trigger: Option<StopTrigger>,   // what made the supervisor kill, if it did
    pub enforced_at: Option<Duration>,       // deadline enforcement committed (§6.3)
    pub termination: TerminationEvidence,
    pub cleanup: Cleanup,                    // §4.4
    pub violations: Vec<Violation>,          // WallDeadline, OutputBudget{stream, offset}
    pub timing: Timing,
}
pub struct TerminationEvidence {             // ≤ 8 attempts
    pub attempts: Vec<KillAttempt>,          // { at: Duration, method: KillMethod, result: Result<(), i32> }
}
pub enum Termination {
    Exited { code: i64 },                    // Windows raw u32; NTSTATUS name attached when known
    Signalled { signal: i32, core_dumped: bool },
    Unknown,                                 // ReapTimeout / reap failure
}
pub enum StopTrigger { WallDeadline, OutputBudget(Stream), Cancelled }
pub struct Timing {
    pub setup: Duration,                     // §3.4
    pub wall: Option<Duration>,              // §6.3, monotonic, all OSes
    pub observer: Observer,                  // Pidfd | Kqueue | Polling | ProcessHandle
}
pub struct SpecRef { pub digest: [u8; 32], pub persisted: Option<RunPath> }
pub struct RunRecord {
    pub spec: SpecRef,
    pub execution: Execution,
    pub stdout: StreamCapture,
    pub stderr: StreamCapture,
    pub measurements: Vec<Measurement>,
    pub limits: Vec<InstalledLimit>,         // §7.2
    pub supervisor_errors: Vec<SupervisorError>, // ≤ 16, deduplicated: Register, Wait, PipeRead, Kill,
                                                 // Reap, ReapTimeout, Accounting, Worker
    pub notes: Vec<Note>,                    // ≤ 16: ObserverFallback, DeadlineGrace, LimitLowered, …
}
```

Every vector in a record is capped and deduplicated, every path field is
≤ 4 KiB serialised (paths are tool-chosen inside the run directory), so the
serialised record is ≤ 64 KiB by construction. A `NotStarted` record has
empty captures.

### 6.2 Milestones and publication

- The record is **finalized** once capture is frozen, cleanup has a result
  and status is known (or `Unknown`), or creation cancellation returns
  `Transferred` (§3.4). It never changes afterwards. `cleanup.log` is the
  bounded initial summary written by the job's persistence at record time;
  late events (a stalled writer returning, a late reap) after persistence
  settles are not appended. If the public future is dropped with no live
  engine to persist it, cleanup finalizes the outcome only in memory and
  publishes it through the ticket (§3.2); it owns no persistence queue.
- The verdict is **derived after** finalization from all fields, never from
  the stop trigger alone.
- Exactly one component finalizes each record: the supervisor, or the
  cleanup service for transferred resources. A §3.4 `Transferred` record is
  a finalized cancellation snapshot; the eventual ticket outcome is separate.
- **Persistence**: each artefact (spec, record, `cleanup.log`, report) is
  one temp file + rename (storage contract) on the job's single persistence
  thread, fed by one queue in enqueue order. Specs are enqueued at
  admission, before process creation; a record **immediately at
  finalization** (t_rec), followed by its first `cleanup.log`; the report at
  the start of report phase S (report contract §2.9). Size bounds: spec
  ≤ 256 KiB, record ≤ 64 KiB, `cleanup.log` ≤ 64 KiB (then a truncation
  marker), report ≤ 64 KiB + 12 KiB × planned executions (report contract
  §3.7; configuration contract caps runs per job).
- **Queue credits**: the queue holds at most **Q = 2 MiB** of credits
  (*fixed*). An artefact is charged 8 KiB (the cost of one write) plus its
  encoded length, known at enqueue for specs and `cleanup.log`, or, for a
  record, which the thread encodes, its 64 KiB bound. The largest charge,
  264 KiB, fits Q, and ≤ 256 artefacts are ever queued. The report takes no
  credits: one queue position is reserved for it at job start and never
  used by another artefact. A charge returns to the pool when the thread's
  write of that artefact returns, published or failed. Credits measure
  in-memory queue capacity only and are distinct from the §5.4 metadata
  reservation, which stands for possible disk bytes; credits never create
  disk headroom and reservations never enter the credit pool. Credit
  release happens at every write return and never waits for reservation
  settlement. Once the thread is unavailable the pool is closed: credits
  returned after that, and those of artefacts it will never write, are not
  reissued.
- **Reservation settlement**, exactly one owner per write (storage
  contract §6.4 commit):
  - *Normal completion*: the write returns before its deadline and before
    the thread is marked unavailable. The persistence thread itself
    commits that artefact's measured disk bytes to retained bytes and
    releases the rest of its reservation at the return, then reports the
    result.
  - *Late completion*: the write returns at or after its deadline, or
    after the thread was marked unavailable. The thread commits nothing
    and reports no publication; it moves the reservation with the measured
    bytes into its return value and exits. Whoever joins the finished
    thread commits them once when it drops that value: the job's
    persistence service if the thread has already finished when the job
    drops it, otherwise the cleanup service after the handover (below).
    Queued artefacts never written release their reservations at the same
    point, uncommitted.

  The thread decides normal or late once, at the return, against its
  deadline and the unavailability flag, so no write is committed twice or
  left uncommitted.
- **Enqueue** takes the charge in call order (no overtaking). When credits
  are short the calling task waits, never blocking a thread and never
  holding the verdict lock, until the first of:
  - credits granted: the artefact is **accepted**;
  - the **credit wait limit**, 1 s after the call: `QueueFull`;
  - the **credit budget** B exhausted: the wall time during which at least
    one enqueue of the job waits reaches B = 10 s + 4 ms × 3 × planned
    executions, fixed at admission (≤ 130 s): `QueueFull`;
  - cancellation intent (§6.4): `Cancelled`;
  - the thread becomes unavailable (a missed deadline, below, or the
    thread has exited): `WorkerUnavailable`.

  A `QueueFull` from either limit makes the queue **nonblocking** for the
  rest of the job: every current wait ends with `QueueFull`, and a later
  enqueue that finds credits short fails at once with `QueueFull`. After
  cancellation intent a later enqueue that finds credits short fails at once
  with `Cancelled`; one that finds credits is accepted, so evidence of
  cancelled runs is persisted whenever it fits. With the thread unavailable
  an enqueue fails at once with `WorkerUnavailable`. A refusal is the
  artefact's settled result and changes only its reference and `persist`
  warning, never the execution's evidence, result or course. The job's
  total credit waiting is ≤ B; with a stalled thread it is ≤ 1 s, because
  the first limit expiry makes the queue nonblocking.
- **Correctness** is the state machine above and below with its *fixed*
  values (Q, the charge, the 1 s limit, B, the 5 s deadlines), for any
  write service times. It is tested with a controlled writer and a
  controlled clock (report contract §9: scripted service times, an
  injected monotonic clock), never with an assumed filesystem rate.
- **Design point** (sizing rationale, not a guarantee). Service
  assumption: while any artefact is queued, the persistence thread hands
  back the credits of one artefact at least every 4 ms. Those 4 ms cover
  the whole per-artefact cycle: encoding (records), the write and its
  syncs, reservation settlement, result publication, credit release, and
  any scheduling gap or lock wait before the next write starts. Under that
  assumption no `QueueFull` and no `Timeout` occur. An enqueue waits only
  while credits are short, so work is pending throughout every wait and
  credits come back at least every 4 ms: total waiting ≤ 3 × planned
  executions × 4 ms < B. Enqueues are served in call order and ≤ 5
  producers enqueue concurrently (4 admitted runs and admission, §3.3), so
  a wait ends once ≤ 5 × 264 KiB has been freed in charges of ≥ 8 KiB,
  ≤ 165 cycles (660 ms) < 1 s. An accepted artefact waits ≤ 1 s for
  credits and then behind ≤ 256 queued artefacts (≈ 1 s), inside its 5 s.
  A bound on write calls alone is not enough: one slow cycle (a 1.2 s
  write, or a 1.2 s pause between writes, while the queue is full) ends
  waits with `QueueFull`, and aggregate throughput does not prevent it.
  Real hosts give no such bound, so there the outcomes are those the state
  machine yields from the actual service: backpressure (≤ B in total),
  then `QueueFull`, or `Timeout` and `WorkerUnavailable`. A successful
  real-filesystem run (report contract §9.2) shows only that that run
  published everything, not that the host meets the service assumption.
- **Publication deadline**: each accepted artefact's deadline is its
  enqueue **call** + 5 s, the credit wait included (for records t_rec + 5 s,
  matching §5.5 and §10.1); the report's is its enqueue at the start of S
  + 5 s. A refused artefact has no deadline. When an artefact misses its
  deadline the job records `PersistError{artefact, Timeout | Io(err)}`,
  marks the persistence thread **unavailable**, fails every still-pending
  artefact of that job at once with `PersistError{Timeout}` and ends every
  current credit wait with `WorkerUnavailable`. Later artefacts fail with
  `PersistError{WorkerUnavailable}` (no replacement thread); the job still
  finishes and prints its in-memory report with `--json`; the deadline does
  not cancel the syscall. The job never names a path it has not seen
  published (in `JobFinished`, the report or a record). A late rename may
  still land (late completion, above); it is harmless and is not appended
  to `cleanup.log` (above):
  it follows a missed deadline, after which the job's queue is unavailable,
  and the cleanup service owns no persistence path. When the job drops its
  persistence service while the thread is unavailable or still owes a
  result, the thread passes to the cleanup service with its thread credit,
  its job-owner clone (lease and byte budget) and the metadata reservations
  of its current and queued artefacts, without a cleanup slot. The service
  never waits for it: each poll joins the thread only once it has finished
  (as for transferred sink workers), then settles the late write as above
  (the one commit of its measured bytes) and releases the rest. A healthy idle thread is not transferred;
  closing its queue ends it, and it returns its own credit. The in-memory
  report stays authoritative for the invocation.
- **Settlement**: a job settles its persistence before finalizing its
  report: it waits, outside the verdict lock, until every artefact it
  enqueued (specs, records, first `cleanup.log` publications) has a result,
  published or `PersistError`. A refused artefact settles at its refusal and
  an accepted one by its deadline. Every enqueue call has returned when
  settlement begins, so it ends at once with no accepted artefact (e.g. a
  build-free `build`) and otherwise by the last accepted enqueue call
  + 5 s, at most 5 s after it begins; it leaves the thread idle or
  unavailable: the report never queues behind another artefact, and with
  the thread unavailable it fails at once. Cancellation ends credit waits but no deadline. The report
  then names a path for every published artefact and an error for every
  other (report contract §2.9 W); the bound to `JobFinished` is report
  contract §2.9 S.

### 6.3 Deadline semantics

The wall deadline is an **observation-based deadline with a fixed 50 ms
grace** on one monotonic clock on every OS.

- `t_created` = monotonic instant when creation returned. The timer fires at
  `t_created + wall_deadline` and is only the enforcement trigger.
- **Measured wall** = monotonic instant the exit was observed − `t_created`,
  on all OSes. Windows `GetProcessTimes` lifetime is kept only as a separate
  measurement (`ProcessLifetime`, §7.1), never used for the verdict.
- **Enforcement:** when the timer fires, the supervisor does one exit check
  (§4). Exit observed → it is an exit. Otherwise it **commits enforcement**
  (`enforced_at`, `stop_trigger = WallDeadline`) and then requests the kill.
  The same check runs whenever exit and timer are ready in one wakeup, so
  selection order never decides.
- **Violation** iff enforcement was committed before exit was observed, or
  measured wall > `wall_deadline + 50 ms`. An exit observed after the commit
  does not revoke it (whether the kill caused that exit is not knowable).
  Measured wall in (`wall_deadline`, `wall_deadline + 50 ms`] without a
  commit → no violation, `Note::DeadlineGrace`.
- Narrowing, stated in the docs: a supervisor stall longer than the grace
  (heavy load, suspended `cpf`) or polling observation (≤ 10 ms, §4.1) can
  produce a violation for a program that finished in time; `Timing.observer`
  records how exit was observed. The grace is a policy tolerance, not a bound
  on observation error, and gives no running program extra time.
- Cancellation observed after the root exited does not change the run; it
  affects the job (§6.4).

### 6.4 Test verdict and job cancellation

Precedence (supersedes the list in design §6.3):

`infrastructure` > `cancelled` > `wall deadline` > `output budget` >
`signal` > `nonzero exit` > `checker failure` (initial feature release) > `wrong answer` >
`unchecked` > `pass`.

- `infrastructure`: `NotStarted` other than `CancelledBeforeStart`, any
  `SupervisorError`, cleanup `Incomplete`, status `Unknown`, or any stdout
  loss of kind `WriteError`, `WriteStalled`, `PipeRead`,
  `NoEofBeforeDrainDeadline` or `ForwardIncomplete`. Each loss kind is judged
  on its own: a budget breach (`exceeded_at`, `BudgetDiscard`) gives the
  output-budget verdict and never excuses another loss. A `Discard` breach
  on a stream that is not compared is a note, not an output-budget outcome
  (report contract §2.2).
- Cleanup `Unverified` does not block a pass; the summary says "cleanup not
  verified".
- A pass requires `Exited{0}`, no violation, stdout complete, no supervisor
  error, cleanup `Confirmed` or `Unverified`, and a comparison that ran and
  matched.
- Comparison after an unsuccessful execution is diagnostic only.
- **Cancellation arbitration**: cancellation records
  **intent** only (flag and time) and stops admission; it never writes a
  verdict. Each test's verdict is derived once, after its records are
  finalized, from execution evidence and the comparison result or
  `comparison skipped`, under one per-job verdict lock that cancellation also
  takes to set intent, so every derivation sees intent either before or
  after. Intent arbitrates check admission per report contract §2.4. A
  check I/O failure is infrastructure unless the check is diagnostic, in
  which case it is evidence only. A test is `cancelled` if its run was
  stopped by cancellation, never started (`CancelledBeforeStart`) or had its
  comparison skipped by cancellation — unless its evidence is
  infrastructure, which keeps precedence. Comparisons skipped after an
  unsuccessful execution publish through the same path. The job verdict
  follows the precedence of report contract §2.7 (`internal_error`, then
  `infrastructure`, then `cancelled`, then the outcome rows): it is
  `cancelled` only if no higher-precedence value applies and some primary
  execution or check was stopped, skipped or refused by cancellation;
  intent that stopped nothing leaves the completed verdict. Every intent
  set before report finalization is recorded with its phase (report
  contract §2.9) and the process status follows report contract §5.2. The
  report is still finalized.

## 7. Limits and measurements

### 7.1 Measurements

```rust
pub struct Measurement {
    pub kind: MetricKind,           // PeakRss | PeakWorkingSet | PeakJobCommit | CpuTime | ProcessLifetime
    pub value: Option<u64>,
    pub unit: Unit,                 // Bytes | Microseconds
    pub scope: Scope,               // Root | RootAndWaitedDescendants | Job
    pub aggregation: Aggregation,   // MaxOfProcesses | Sum | JobPeak
    pub completeness: Completeness, // Final | Partial (cleanup not Confirmed)
    pub method: &'static str,
    pub reason: Option<String>,     // why None / Partial, ≤ 256 bytes
}
```

| Metric | Linux | macOS | Windows |
|---|---|---|---|
| Peak RSS | `wait4` `ru_maxrss` ×1024; root and its waited-for descendants; max over processes (kernel high-water mark, not a simultaneous tree peak) | same, bytes; offered only if Unix accounting validates it, else declared unsupported | — |
| Peak working set | — | — | root process, `PeakWorkingSetSize` |
| Peak job commit | — | — | job, `PeakJobMemoryUsed`, `Final` only after `Confirmed` |
| CPU time | `ru_utime + ru_stime`, root + waited descendants, sum | same | job `TotalUserTime + TotalKernelTime`, sum |
| Process lifetime | — | — | root, `GetProcessTimes` exit − creation (informative only, §6.3) |

### 7.2 Local limits

```rust
pub enum LocalLimit { AddressSpace { bytes: u64 }, JobCommit { bytes: u64 } }
pub struct InstalledLimit {
    pub configured: LocalLimit,     // as requested
    pub inherited: Option<(u64, u64)>, // soft/hard sampled at admission (Unix)
    pub normalised: u64,            // after page rounding
    pub installed: Option<u64>,     // value set for this run, read back where possible
    pub mechanism: &'static str,
    pub outer: OuterRestriction,    // None | Known(u64) | Unknown (e.g. ancestor Windows job)
}
```

- Validation before spawn: at most one per kind; `bytes > 0`; rounded up to
  the page size with checked arithmetic; `RLIM_INFINITY` and values ≥ the
  platform maximum rejected. Unsupported on this platform →
  `NotStarted{UnsupportedLimit}`; installation failing for this run →
  `NotStarted{Limit(errno)}`; never silently ignored. A `doctor` probe is
  informative only; installation is checked per run.
- `AddressSpace`: soft = hard = min(normalised, inherited hard). This may
  **raise** an inherited soft limit (intended: the requested limit is the one
  in effect); a lower inherited hard limit is recorded in `installed` with
  `Note::LimitLowered`. An unprivileged child cannot raise it.
- `JobCommit`: set on the run's job and read back with
  `QueryInformationJobObject`; an ancestor job may impose a lower limit that
  cannot always be read → `outer = Unknown`.
- Published judge limits are never converted into a local limit implicitly;
  The configuration contract defines the explicit request. A program failing under a local limit gets
  its ordinary outcome; the summary may name the limit but never says MLE.
- `doctor` distinguishes compiled support, run-time probe result, and
  deferred features (cgroup accounting/containment, `--jobs`, build cache:
  *unavailable feature*).

## 8. Console interaction

### 8.1 Raw `cpf run`

- Raw mode is one run in a short-lived `cpf` process.
- Output: readers → bounded queue (1 MiB per stream) → **forwarder thread**
  writing to `cpf`'s stdout/stderr. When the queue is full the reader stops
  reading (back-pressure); deadline and cancellation are unaffected. At the
  console flush deadline (2 s from drain start) unforwarded bytes are dropped
  (`ForwardIncomplete`) and the forwarder is abandoned: this is **logical
  forwarding abandonment** only. The run then finalizes, and its record and
  report are persisted, under the ordinary rules (§§5.2, 6.2) before `cpf`
  exits. **Raw-mode exception:** at exit, physical cleanup still
  pending (a stalled writer, an unreaped root after `ReapTimeout`) is
  abandoned to process teardown; an unreaped root is reparented to init. The
  record says so.
- Stdin: one helper thread reads `cpf`'s stdin into a 64 KiB queue → pipe to
  the child; EOF closes the child's stdin; a broken pipe stops forwarding. It
  may stay blocked in `read` until `cpf` exits; since raw mode is the whole
  process, no later interaction can lose input to it. The child is not the
  terminal's foreground group and must not read the terminal directly.
- `--json` / `--events` are rejected with raw mode. Exit statuses: report
  contract §5.5 (fixed statuses, the child's status is reported, never
  passed through).

### 8.2 Ctrl-C

- Unix: a self-pipe signal listener turns `SIGINT`/`SIGTERM` into a message
  handled on a dedicated control thread (neither the executor nor the
  creation thread). Windows: the `SetConsoleCtrlHandler` callback (its own
  thread) sends the same message. No cancellation logic runs in a signal
  handler.
- First notification: cancel the current job. On Unix, when cancellation
  then decides the exit status, `cpf` kills every registered run through its
  signalling capability (§3.1.6) immediately before re-raising the signal
  (report contract §5.3), as force exit does; this covers a run transferred during committed creation whose
  creation thread has not yet run its kill path (§3.4). Any later
  notification ≥ 100 ms
  after the first: **force exit** — take the live-run registry lock (never
  held across creation or any blocking call), kill every registered run
  through its signalling capability (§3.1.6), terminate per report contract
  §5.3 (Unix: re-raise the forcing signal; otherwise 6). No further
  intentional write to stdout or stderr begins after force exit starts.
  Creation in progress is not awaited (§3.4). Force exit is the one path
  where the final report is **not** guaranteed; records already persisted
  remain.

## 9. Jobs, events, report

```rust
pub enum Event {
    JobStarted  { seq: u64, job: JobId, kind: JobKind },
    RunStarted  { seq: u64, run: RunId, stage: Stage, digest: String },
    OutputChunk { seq: u64, run: RunId, stream: Stream, bytes: Bytes },   // fresh ≤ 4 KiB buffer
    RunFinished { seq: u64, run: RunId, summary: RunSummary },            // ≤ 7 KiB (report contract §3.7)
    Progress    { seq: u64, done: u32, total: u32 },
    JobFinished { seq: u64, job: JobId, verdict: JobVerdict, report: Option<ReportId>, dropped: u64 }, // ≤ 4 KiB
}
```

- Every event carries a job-wide sequence number. When the transport budget is
  full, events other than `JobFinished` are dropped and counted. `seq`
  starts at 0 and is assigned in one critical section with the
  enqueue-or-drop decision; `JobFinished` takes the next number after the
  main channel is closed. The terminal slot is filled after report
  persistence (report contract §2.9 S), so `JobFinished.report` reflects
  it. `RunStarted` carries `execution` and `role` on the wire (report
  contract §7.3).
- **Terminal delivery**: the job stops all its producers
  (supervisors finished or transferred; transferred runs no longer emit job
  events), **closes the main channel**, computes `dropped`, then publishes
  `JobFinished` into a reserved single slot. Consumers drain the main channel
  until it is closed, then read the slot; `seq` may have gaps (drops), so
  closure, not `seq`, is the barrier. Guarantee: retained for a connected
  consumer until read; a disconnected consumer makes emission a no-op and
  never fails the job.
- `report` is a bounded identifier, set only after the report file was seen
  published; its path is derived from the job's run directory (storage contract).
  Otherwise `None`; the reason is in the delivered document's
  `persistence` member (report contract §4.11) and on stderr, never in
  `report.json`, which cannot contain its own persistence result.
- **Report:** finalized in memory → persisted (bounded by the persistence
  deadline; failure is `PersistError`, not a hang) → transported (`--json` to
  stdout). With `--json` the in-memory report is written to stdout **even if
  persistence failed**. Size is bounded by report contract §3.7 and the
  configuration contract's limit on runs per job. Transport failure (closed
  stdout, `EPIPE`) is a delivery failure; the process status follows report
  contract §5.2. A stalled stdout blocks the final write as
  in any CLI; all supervision is finished by then and force exit stays
  available.
- Guarantee, narrowed: one finalized in-memory report per job, except force
  exit and crashes of `cpf`; persistence and transport are separate,
  individually reported results.

## 9a. Connected runs (supervisor constraints)

Interactive judging follows the CLI foundation; connected-run mechanics
belong to the supervisor, with their contract in `../design-checkers.md` §3.
This section fixes the constraints that the process supervisor must respect.

**Binding for the supervisor:**

- `StdinSource::Connected(PeerRef)`, `Sink::Connected(PeerRef)` and
  `deadline_group` exist and are rejected (`NotStarted{Unsupported}`) until
  connected-run support is implemented.
- The ownership model admits a future owner of connection endpoints other
  than either run (§3.1 rule 1 applies unchanged).
- Admission (§3.3) can reserve two runs atomically: slots, credits and
  workers for both, or nothing.
- Nothing in §§3–6 assumes a run's stdin is a file or that its stdout ends in
  a sink owned by that run alone.

**Provisional (the connected-run contract decides):**

- Relay mode (`cpf` forwards each direction and records a bounded transcript)
  is the only mode while budgets or transcripts are required; direct
  child-to-child pipes are deferred until their budget semantics are chosen,
  since `cpf` cannot count bytes it does not see.
- Per-direction forwarding capacity, with transcript writing unable to stall
  delivery; a blocked destination stalls only its own direction.
- Group deadline origin and violation attribution, start-up rollback of a
  half-created pair, half-close and endpoint ownership, peer-shutdown policy.
- Raw outcomes and observation timestamps of both runs are recorded without
  claiming an exact exit order or `SIGPIPE` causality; verdicts belong to the
  interactive spec.

## 10. Acceptance criteria for supervisor tests

### 10.1 Deadlines, once

Milestones: timer fire, kill requested (t_k), root exit observed (t_r),
cleanup result (t_c), drain start (t_drain_start, §5.2), reader stopped
(t_s), capture frozen (t_f), record finalized (t_rec), record persisted
(t_p), resources released (t_rel). Each bound is a **fixed policy deadline**
(exact, from §5.5) plus a **test allowance** in brackets. Allowances are for
the Linux container, ×2 on hosted Linux CI and ×4 on macOS and Windows CI,
an assumption about hosted runners; policy deadlines are never scaled.

| Path | Bound |
|---|---|
| timer fire → t_k | [50 ms] |
| t_k → t_r, normal tree | [250 ms] |
| t_r → t_c | cleanup deadline 1 s [+ 50 ms] |
| t_c → t_drain_start, immediate reap | [50 ms] (reap follows verification) |
| t_c → t_drain_start, consuming-wait retries (Linux or macOS) | ≤ remaining time to the reap deadline (2 s from t_k, or from t_r without a prior kill) [+ 50 ms]; past it the run is transferred and the reap-timeout row applies |
| t_drain_start → t_s, pipes closed by the tree | [100 ms] |
| t_drain_start → t_s, pipe held outside containment (Unix `setsid`) | drain 500 ms [+ 100 ms]; `NoEofBeforeDrainDeadline` |
| t_s → t_f, healthy sinks | [100 ms] |
| t_drain_start → t_f, stalled sink | sink finalisation 5 s [+ 100 ms]; `WriteStalled` |
| t_f → t_rec | [100 ms] |
| t_rec → t_p | healthy, **idle queue** (no artefact queued or being written at the enqueue call): [100 ms]; accepted behind a backlog or after a credit wait: by the persistence deadline, t_rec + 5 s (credit wait + queue residence + write) [+ 100 ms]; stalled disk: persistence 5 s [+ 100 ms], `PersistError` |
| t_rec → t_rel | healthy [100 ms]; stalled writer: [100 ms] after it returns |
| normal exit, **healthy path only** (no pre-exit kill, immediate reap, prompt EOF, healthy sinks; the post-exit containment kill still runs): t_r → t_f | (t_c − t_r) [+ 250 ms]. Escaped-pipe, retry and stalled-sink paths use their rows above |
| reap-timeout path (Linux opt-in, uninterruptible fixture) | transfer at t_k + reap 2 s [+ 100 ms]; t_rec ≤ transfer + drain 500 ms [+ 100 ms] |

### 10.2 Fixtures and invariants

- **Readiness and identity:** `cpf-fixture` gets a test-only control channel
  from the harness (an extra descriptor/handle, outside §3.5's rule,
  documented as test-only). Each fixture process writes
  `READY <pid> <start-id>\n` (Linux `/proc/<pid>/stat` start time; macOS
  `proc_pidinfo` start time; Windows creation time) and **blocks until the
  harness acknowledges**, so identities are pinned while alive; on Windows
  the harness also holds a handle to each descendant before acknowledging.
  Messages are ≤ 256 bytes; readiness and acknowledgement time out after 5 s,
  and on timeout the harness kills the fixture's containment unit and fails
  the test.
- **Gone** is two separate assertions: *no live identity* — Unix
  `kill(pid, 0) = ESRCH`, a different start-id, or state `Z` (a killed
  descendant may stay a zombie until someone reaps it); Windows the held
  handle is signalled. *Entry disappeared* — only in tests where the harness
  is a Linux child subreaper (`PR_SET_CHILD_SUBREAPER`) and reaps the
  descendants itself.
- **Leak checks:** warm up 50 runs, then 5 batches of 100; counts taken only
  at a quiescence point (all runs at resources released, cleanup service
  empty). fd / handle counts, thread counts and service occupancy MUST be
  equal at every batch boundary; the supervisor's owned-resource debug counter
  MUST be 0.
- **Memory:** fixture allocates N MiB and writes one byte per page.
  Linux/macOS peak RSS in [N, N + 16 MiB + baseline], baseline = N = 0 in the
  same test. Windows peak working set in the same interval (touching pages
  makes them resident; trimming under memory pressure is the known risk, so
  N ≤ 256 MiB); job commit in [N, N + 32 MiB + baseline] (commit includes
  image and CRT heap reserve). macOS RSS requires validation of the accounting method first.
- **Cap boundaries** at budget 4096: 4095 / 4096 / 4097 bytes, both streams,
  `File` and `Discard` sinks, `Kill` and `Discard`; `stored` never exceeds
  4096.
- **Flood:** 64 MiB on both streams concurrently (stress suite, required in
  CI).

### 10.3 Required cases (each needs a fixture, injection point, invariant and bound)

Immediate exit before/during/after observer registration; forced pidfd
failure → polling; `waitid` / `wait4` errors incl. `ECHILD` (injected) →
§3.1.6; worker pre-creation failure → `NotStarted{Worker}` (injected); limit
failure in `pre_exec` (phase-tagged) vs other spawn failure (generic); kqueue
success vs error receipt; Windows `WAIT_FAILED` (injected);
cancellation before commit, during creation, after commit; cancellation and
force exit during a blocked creation (injected); drop of the public future in
every §3.2 state; root exit with descendants holding stdout / stderr /
neither; late `OutputBudget` after root exit; timer and exit ready in one
wakeup (injected supervisor delay, both orders); pending overlapped read
during cancellation and completion racing `CancelIoEx`; `ConnectNamedPipe`
pending and failing (injected); permanently stalled sink (FUSE/stub) with its
reservation held until the writer returns; sink write error (tiny tmpfs on
Linux, failing stub elsewhere); report persistence stalled and failing; event
consumer stalled for 10 000 runs and terminal ordering by `seq`; cleanup
capacity and thread-credit exhaustion (5th concurrent admission refused);
transferred run's logical record within its bound while physical cleanup
continues; enumeration error, truncation (injected small buffer), short
`proc_pidinfo` return and zombie members on Linux and macOS; `/proc`
prerequisite failures and `hidepid` mounts (injected) → `Unverified`; Windows
zero-byte read completion and `ERROR_BROKEN_PIPE` as EOF; late persistence
rename after `PersistError`; persistence credits with a controlled writer
and clock, throughput on a real filesystem (platform evidence), queue
saturation then report, permanently stalled persistence thread, normal and
late completion settlement and cancellation in a credit wait (report
contract §9.2); budget breach plus an independent write error
(both reported); `ACTIVE_PROCESS_ZERO` never delivered
(port not associated) still `Confirmed` by polling; Windows inside a
compatible outer job and inside one forbidding nesting; address-space limit
with a lower inherited hard limit; `Connected` variants and `deadline_group`
rejected; real Ctrl-C and second Ctrl-C (after CLI integration).

## 11. Guarantee caveats and rationale

- Cleanup confirmation is an observation by a stated algorithm, with
  `Unverified` where it cannot run; `Unverified` does not block a pass. It is
  not a proof against escape or reaping by third parties.
- Deadlines use a fixed 50 ms grace and deterministic reconciliation. Unix
  supervisor stalls can still produce false violations, recorded via
  `method`.
- A stalled stdout may block final transport after supervision has finished;
  persistence is bounded separately.
- Neither orderly exit nor force exit waits for an in-progress creation. On
  Unix an unregistered child may survive `cpf` for an unbounded time, at most
  one per job (§3.4).
- Hosted Linux CI allowances are the Linux allowances ×2 and macOS/Windows
  CI allowances ×4, an assumption about hosted runners; policy deadlines are
  never scaled.
- Fixture readiness uses a bounded, test-only control channel outside the
  inheritance rule instead of lossy `OutputChunk` events.
- Unix observers need no thread (pidfd, kqueue or polling); all workers exist
  before any child, and one pre-created polling thread is the only recovery
  route.
