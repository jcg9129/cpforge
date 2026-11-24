# Report contract

Status: v1 (decisions of 2026-11-21, §11). Normative for the `report` module, the result
derivation in `jobs`, and the process exit status of every `cpf` command.
It is the **report transport** and **report schema** that the other
contracts refer to: execution contract §§6.2, 6.4, 8, 9; storage contract
§§5.2, 5.3, 5.7, 6.1, 6.2, 6.5; configuration contract §§8.1–8.3, 10;
`../design-checkers.md` (CLI exit codes); design §6.1 and §8. References
labelled *E*, *S*, *C*, *compare spec*, *checkers design* and *workspace
design* point to `execution.md`, `storage.md`, `config.md`,
`../compare-spec.md`, `../design-checkers.md` and `../design-workspace.md`.

MUST / MUST NOT / never are requirements. This contract derives results
from records the other contracts define, encodes them, and maps them to
exit statuses. Where it needs a change in another contract, §10 gives the
amendment text.

**Organisation amendment: draft.** The `ls`, `search` and `notes`
documents, the status document's handling of organisation fields (§6.1),
and the exit rows of `tag`, `untag`, `mark`, `notes`, `ls`, `search`
(§5.1, §5.5); storage contract §5.9. Marked *(organisation, draft)*; merged
into schema 1 by the final reconciliation.

**Scope of schema 1.** Built-in batch comparison only (compare spec):
`batch` problems, comparators `exact`, `tokens`, `lines`, `float`.
Checkers, interactors, scores, presentation errors and connected runs are
**not** part of schema 1; they arrive with schema 2 (§3.6), which will
extend the check outcome, the verdict set, stages and roles. Names are
chosen to stay compatible with checkers design §1, but schema 1 readers
need to know only the values listed here.

## 1. Terms

- **Job kinds**: `build` (`cpf build`), `test` (`cpf test`), `run` (raw
  `cpf run`, E §8.1), `cmd` (`cpf cmd <name>`), `hook` (the `on_switch`
  hooks of a successful `cpf switch`, C §8.3, §2.10).
- **Planned execution**: one entry of the plan (C §8.2 planned runs):
  builds, tests × repeats (or repeats), the raw run, hooks. It has a **plan
  index** (from 0, plan order) and a **coverage** (§2.1) independent of its
  outcome. An execution that is admitted also gets a **run id** (§3.4).
- **Primary executions**: for job kinds `build`, `test`, `run`, `cmd`, every
  planned execution except hooks; for kind `hook`, its hook executions.
  Only primary executions aggregate into the job verdict. The `after_test`
  hooks of a `test` job never do.
- **Run class** (§2.2), **check outcome** (§2.4), **test verdict** (§2.5),
  **job verdict** (§2.7).
- **Report**: the job's in-memory document (E §9), immutable from phase F
  (§2.9); `report.json` is its persisted copy (S §6.1); the **stdout
  document** is the report plus `persistence` (§4.11).
- **Command failure class** of any command (job or not): `usage`,
  `config`, `workspace`, `partial`, `cancelled`, `internal`, or none.
  A **refusal** is a job command that fails with class `config`,
  `workspace`, `cancelled` or `internal` before its job is **ready**
  (S §6.2); it has no report file and, in machine mode, writes a refusal
  document (§4.12).
- **Machine output**: `--json` (one document) or `--events` (an event
  stream ending with the document, §7). Everything else is **human
  output** (§8).
- **Injection point**: a named, feature-gated test hook (like the storage
  crash points, S §10) at which a harness can block, fail or observe; §9
  fixtures use only these, never timing.

## 2. Result model

All derivations are pure functions of finalized records, the frozen
manifest and the cancellation intent read under the job's **verdict lock**
(E §6.4). The rules below generate only the combinations their tables list.
A combination outside the tables can arise only from a defect (a broken
record, a bug); it is an **orphan**. Orphans are represented without
inventing evidence: the observed coverage and record are serialized as
observed; the derived subresult that cannot be computed takes its
`internal_error` form (build result §4.4, test verdict and check §4.6,
execution `invariant` §4.5, hook outcome §4.9); `result.internal` names the
first invariant and its locus (§4.8).

**Missing or underivable evidence** is never replaced by an ordinary
outcome:

- An admitted execution whose record was never produced (e.g. a panic in
  its supervisor before finalization) has coverage `admitted` (§2.1),
  `invariant: record_missing`, `record` and `summary` `null` (§4.5); no
  class is derived (`class: null`, counted as `unknown`, §4.8).
- An execution whose record exists (coverage `started` or `not_started`)
  but whose summary cannot be built (class derivation or summary
  construction panicked, or the record violates an invariant of E §6.1)
  keeps its observed coverage and its `record` reference, has `summary`
  `null` and `invariant: summary_underivable`, and keeps `class` if the
  class was derived before the failure (`class: null` otherwise, counted
  under `unknown`, §4.8); the record itself remains the evidence.
- A check that cannot be derived (its input record is missing, or the
  comparison or derivation panicked) is `internal_error{invariant}` (§4.6);
  its test verdict is `internal_error`.
- A hook execution in either situation has the hook outcome
  `internal_error{invariant}` (§4.9).

**Internal failures** are orphans and caught panics (§5.4). One rule each:

- **Recorded before V** (phase P, including a panic on a run, check or
  derivation thread): `primary_verdict = verdict = internal_error`
  (§2.7 row 1). No fact is published (§2.9 A eligibility), no `after_test`
  hook runs.
- **Recorded after V and before F** (phases A, H, W, E): `primary_verdict`
  stays as frozen; `verdict = internal_error`. A fact already committed
  stays published; hooks not yet started are `skipped{job_not_eligible}`.
- **After F**: the report is frozen and never changed; see §5.4 for what is
  written.

### 2.1 Coverage

| Coverage | Meaning | Record |
|---|---|---|
| `started` | E §6.1 `Execution::Started` | yes |
| `transferred` | cancelled during committed creation; whether a child exists is unknown at return (E §3.4) | yes (unobserved captures) |
| `not_started{reason}` | admitted, `Execution::NotStarted{reason}` (E §6.1 reasons, including `cancelled_before_start`) | yes (empty captures) |
| `skipped{reason}` | never admitted: no run id, no record | no |
| `admitted` | admitted (run id allocated) but no record was ever produced; only reachable through an internal failure (§2) | no |

`skipped.reason`, each legal only under its condition:

| Reason | Condition |
|---|---|
| `build_not_succeeded` | it depends on a build whose result is not `succeeded` |
| `admission_closed` | an earlier primary execution is `not_started{retention_budget}` (E §5.4) |
| `stop_policy` | job kind `cmd`, `keep_going = false`, an earlier primary execution failed (C §8.2) |
| `cancelled` | cancellation intent was set before its admission |
| `job_not_eligible` | an `after_test` hook, and phase H does not run (§2.9) |

- **Partial**: `result.partial = true` iff some **primary** execution is not
  `started` (`admitted` included: it is not shown to have started). Hook coverage of a `test` job never makes it partial.
- Reconciliation, per population (primary, hooks): `planned = started +
  not_started + transferred + skipped + admitted`; `planned_primary + planned_hooks` equals C §8.2's
  planned-run count. Hooks excluded by trust, the switch or the depth guard
  (C §8.3) are not planned; they appear in `hooks[]` as `not_planned`.

### 2.2 Run class

For an execution with a record, first matching row:

| # | Condition | Class |
|---|---|---|
| 1 | E §6.4 infrastructure evidence: `NotStarted` other than `CancelledBeforeStart`; any `SupervisorError`; cleanup `Incomplete`; status `Unknown`; any **stdout** loss of kind `WriteError`, `WriteStalled`, `PipeRead`, `NoEofBeforeDrainDeadline`, `ForwardIncomplete` | `infrastructure{causes}` |
| 2 | `Transferred`, `NotStarted{CancelledBeforeStart}`, or `stop_trigger = Cancelled` | `cancelled` |
| 3 | violation `WallDeadline` (E §6.3) | `wall_deadline` |
| 4 | `stop_trigger = OutputBudget(_)` (a `Kill` breach), or a breach (`exceeded_at` set, or `BudgetDiscard`) on a **consumed stream** | `output_budget{stream, streams}` |
| 5 | `Signalled{signal, core_dumped}` | `signal{signal}` |
| 6 | `Exited{code}`, `code ≠ 0` | `nonzero_exit{code}` |
| 7 | `Exited{0}` | `exited_zero` |

- **Consumed stream**: the stdout of a test run. A `Discard` breach on a
  stream that is not consumed changes no class (note `truncated_stdout` /
  `truncated_stderr`, decision D8). A `Kill` breach is always row 4.
- `streams` lists every stream meeting row 4, stdout first; `stream` is its
  first element. The breach itself is evidence in `violations` and the
  stream's `exceeded_at`, never a cause.
- **Causes** (row 1) are structured objects (§4.5 `Cause`), deduplicated
  by the key (variant, `stream`, sub-kind) and listed in this fixed order:
  `not_started`, `status_unknown`, `cleanup_incomplete`,
  `supervisor_error` (sub-kinds `register`, `wait`, `pipe_read`, `kill`,
  `reap`, `reap_timeout`, `accounting`, `worker`), `capture_loss` (stream
  `stdout` before `stderr`; sub-kinds `write_error`, `write_stalled`,
  `pipe_read`, `no_eof_before_drain_deadline`, `forward_incomplete`). The
  first 8 are listed, then `causes_dropped`; the first is the primary
  cause.
- Cleanup `Unverified` (note `cleanup_unverified`) and `CwdOverflow` (note
  `cwd_overflow`) change no class. A program failing under a local limit
  gets its ordinary class; there is no memory class (§2.8).

### 2.3 Build result

| Build coverage / class | Post-build check (C §3 phase 4), in this order | Build result |
|---|---|---|
| `skipped{cancelled}` | – | `cancelled` |
| `infrastructure` | – | `infrastructure{causes}` |
| `cancelled` | – | `cancelled` |
| `wall_deadline`, `output_budget`, `signal`, `nonzero_exit` | – | `failed{cause: <class>}` |
| `exited_zero` | declared outputs missing | `failed{cause: missing_output{names}}` |
| `exited_zero` | generated program invalid (C §4.5) | `failed{cause: generated_program_invalid}` |
| `exited_zero` | output beyond the build cap (S §6.4) | `failed{cause: output_too_large}` |
| `exited_zero` | all pass | `succeeded` |

Any other build coverage is an orphan (`internal_error{invariant}`). A
build that is not `succeeded` starts nothing that depends on it (design
§6.6); its dependants are `skipped{build_not_succeeded}`.

`cpf build` on a build-free target (e.g. Python without a build stage)
performs no executions and yields job verdict `succeeded` (§2.7),
`build: null` (§4.4), and exit status 0 (§5.2).

### 2.4 Check outcome (test jobs)

Evaluated once per test, after its record (if any) is finalized:

1. **No record** (coverage `skipped`): check `not_admitted{cancelled}` if
   the coverage reason is `cancelled`, else `not_admitted{run_not_eligible}`.
2. **Eligibility.** Coverage `not_started` (any reason, including
   `cancelled_before_start`), or class `infrastructure` or `cancelled` →
   `not_admitted{run_not_eligible}`.
3. **Admission under cancellation arbitration** (verdict lock, E §6.4):
   intent already set → `not_admitted{cancelled}`; otherwise admitted, and
   intent set later does not stop it (in-process, bounded, compare spec
   §1.3). No comparison of any kind, diagnostic included, is admitted after
   intent.
4. **Answer state** (frozen manifest, S §7): `absent` →
   `unchecked{no_answer}`; nothing runs.
5. **Comparison** (compare spec §1.1, answer validated first):

| Comparator result | Check outcome |
|---|---|
| answer invalid (compare spec §5.5) | `checker_failed{class: test_data_error, reason: invalid_answer, diag}` |
| mismatch | `failed{fail: wrong_answer, reason, diag}` |
| match, capture incomplete | `failed{fail: wrong_answer, reason: incomplete_output, diag}` |
| match, capture complete | `passed` |
| reading the capture or the frozen answer failed (I/O) | `check_error{os_error}` |

- A check admitted after a class other than `exited_zero` is
  **diagnostic** (`diagnostic: true`, E §6.4): whatever its ordinary
  outcome, including `check_error`, it never decides the verdict. A
  diagnostic check that ends in `internal_error` (the comparison or its
  derivation panicked) is not an ordinary outcome: it makes the test verdict
  `internal_error` (§2.5), like any internal failure before V.
- A missing answer file never reaches this machine: it is an unhealthy
  catalog entry and refuses the job (S §8.1, exit 8).

### 2.5 Test verdict

Over (coverage, class, check); first matching row. Every combination
§§2.1–2.4 can produce is listed.

| Coverage | Class | Check | Test verdict |
|---|---|---|---|
| `skipped{cancelled}` | – | `not_admitted{cancelled}` | `cancelled` |
| `skipped{build_not_succeeded \| admission_closed}` | – | `not_admitted{run_not_eligible}` | `not_run{reason}` |
| any | any or none | `internal_error{invariant}` | `internal_error` |
| `admitted` | none | `internal_error{record_missing}` | `internal_error` |
| `not_started{≠ cancelled_before_start}` | `infrastructure` | `not_admitted{run_not_eligible}` | `infrastructure` |
| `not_started{cancelled_before_start}` | `cancelled` | `not_admitted{run_not_eligible}` | `cancelled` |
| `started` | `infrastructure` | `not_admitted{run_not_eligible}` | `infrastructure` |
| `started` | `cancelled` | `not_admitted{run_not_eligible}` | `cancelled` |
| `started` | any other | `not_admitted{cancelled}` | `cancelled` (E §6.4: comparison skipped by cancellation) |
| `started` | `wall_deadline`, `output_budget`, `signal`, `nonzero_exit` | `unchecked`, `passed`, `failed`, `checker_failed`, `check_error` (all diagnostic) | the class |
| `started` | `exited_zero` | `check_error` | `infrastructure` |
| `started` | `exited_zero` | `checker_failed` | `checker_failed` |
| `started` | `exited_zero` | `failed{reason ≠ incomplete_output}` | `wrong_answer` |
| `started` | `exited_zero` | `unchecked` | `unchecked` |
| `started` | `exited_zero` | `passed` | `passed` |

Orphans include: `exited_zero` with `failed{incomplete_output}` (an
incomplete capture is a row-1 loss or a row-4 breach, §2.2), a `skipped`
reason whose condition (§2.1) fails, `stop_policy` or `job_not_eligible`
on a test. An unchecked answer never masks a cancelled, skipped or
infrastructure test (earlier rows precede it).

### 2.6 Outcomes of `cmd`, `run`, hooks

- `cmd` and raw `run` executions: outcome = coverage plus class; an
  execution **failed** (C §8.2) iff it has a record and its class is not
  `exited_zero`. `skipped{stop_policy}` follows the first failure without
  `keep_going`. No check outcome.
- Hook executions: `succeeded` iff `exited_zero`; else `failed{class}`,
  `cancelled`, `skipped{reason}` (the **ordinary** outcomes) or
  `internal_error{invariant}`. In a `test` job an ordinary non-`succeeded`
  outcome is a `hook` warning (C §8.3), never part of the verdict or
  status. A hook `internal_error` is an internal failure (§2): recorded in
  phase H, after V, it sets `verdict = internal_error` (primary verdict
  unchanged) and the status 70 (§5.2 row 3). In a `hook` job the hooks are
  the primary executions (§2.10).

### 2.7 Job verdict

`result.primary_verdict` is computed at phase V over **primary executions
only**, first matching value:

1. `internal_error`: any internal failure recorded before V (§2).
2. `infrastructure`: the build result, any test verdict, or any `cmd`,
   `run` or `hook`-job class is `infrastructure`.
3. `cancelled`: the build result, any test verdict, any such class, or any
   primary coverage is cancelled (`skipped{cancelled}`,
   `not_started{cancelled_before_start}`).
4. `build_failed`: the build result is `failed`.
5. `wall_deadline`, 6. `output_budget`, 7. `signal`, 8. `nonzero_exit`: the
   first present among test verdicts (`test`) or classes (other kinds).
9. `checker_failed`, 10. `wrong_answer`, 11. `unchecked`: among test
   verdicts.
12. `no_tests`: a `test` job with zero selected tests (decision D7: still
    built; a build failure gives `build_failed`).
13. `passed` (`test`) or `succeeded` (`build`, `run`, `cmd`, `hook`).

- **Intent in phase P that cancelled nothing.** If intent was set during P
  but every primary execution had already been admitted and every check
  admitted (nothing was stopped, skipped or refused), no row 3 condition
  holds and the primary verdict is the completed one (e.g. `passed`): the
  evidence is complete, so the report says so. The intent is still
  recorded (`result.cancellation`, phase `primary`), prevents fact
  publication and hooks (§2.9), and selects the cancellation status
  (§5.2).
- `result.verdict` equals `primary_verdict`, except `internal_error` when an
  internal failure is recorded after V and before F (§2).
- `passed` means at least one selected test and every test verdict
  `passed`. Persistence, durability and cleanup warnings, fact outcomes
  and ordinary `after_test` hook outcomes never change a verdict; a hook
  `internal_error` does (§2.6).
- A refusal has the verdict `refused`, outside this order (§4.12).

### 2.8 Display labels and summary rules

Human output uses these labels; the report stores tokens, never labels.

| Verdict / result | Label | Qualifier |
|---|---|---|
| `passed` | `OK` | comparator; "cleanup not verified" when noted |
| `wrong_answer` | `WA` | first difference from `diag` |
| `unchecked` | `UNCHECKED` | "no answer" |
| `checker_failed` | `CHECKER-FAILED` | "invalid answer" |
| `wall_deadline` | `TLE` | "wall deadline <d>", measured wall if known |
| `output_budget` | `OLE` | streams and budget |
| `signal` | `RE` | signal name or number; "core dumped" |
| `nonzero_exit` | `RE` | exit code; Windows NTSTATUS name when known |
| `cancelled` | `CANCELLED` | |
| `infrastructure` | `INFRA` | primary cause |
| `internal_error` | `INTERNAL` | invariant |
| `not_run` | `NOT RUN` | reason |
| build `failed` | `CE` | cause |
| `succeeded` (`run`, `cmd`, hook execution) | `OK` | |

The label set is exhaustive. Summary templates (design §6.1), each fixed
and table-tested (§9.5):

- Job summary: "<passed>/<selected> passed (<comparator> comparison)"; no
  template contains "accepted" or "correct".
- `unchecked`: "<n> unchecked (no answer): not verified"; `no_tests`: "no
  tests: nothing verified"; a completed verdict with recorded intent adds
  "(interrupted after completion)".
- A run under a local limit is labelled by its class; the qualifier may
  name the installed limit (E §7.2).
- `TLE` refers to the local wall deadline; a published judge limit is shown
  separately and never claimed as met or exceeded.
- A diagnostic check is shown after the run's label as "(output matched)" or
  "(output differs: …)", never as `OK` or `WA`.
- Measurements show metric, scope and method; a `null` value shows its
  reason.

### 2.9 Finalization phases

Order: **P → V → A → H → W → E → F → S → D**. *(lock)* marks a step taken
under the verdict lock, atomic with respect to cancellation intent.

- **P Primary execution.** Each test verdict is derived once its record is
  final (§§2.4–2.5). Intent stops admission and arbitrates checks (E §6.4).
- **V Primary freeze** *(lock)*: after the last primary derivation,
  `primary_verdict` (§2.7) is computed and frozen.
- **A Fact publication** (`test` jobs, S §6.5 as amended). **Eligible** iff
  `primary_verdict` ∉ {`cancelled`, `infrastructure`, `internal_error`};
  otherwise `not_attempted{not_completed}`. An eligible job, under the
  workspace lock and after classifying `job-seq` and the existing fact,
  performs the **cancellation cutoff** *(lock)*: one read of intent; set →
  `not_attempted{cancelled}`, nothing written. Not set → the atomic replace
  (the **commit point**) follows and its outcome stands. Intent set between
  the cutoff and the commit, or after the commit, never prevents, undoes or
  hides the replace. Facts are never
  rolled back.
- **H Hooks** (`after_test` of a `test` job, C §8.3): each hook start checks
  *(lock)* that `primary_verdict` ∉ {`cancelled`, `infrastructure`,
  `internal_error`} and no intent is set; otherwise it and all later hooks
  are `skipped{job_not_eligible}` or `skipped{cancelled}`. Intent during a
  hook cancels it.
- **W Settlement barrier** (outside the verdict lock): the job waits until
  every artefact it enqueued for persistence (specs, records, first
  `cleanup.log` publications, E §6.2) has a result, published or
  `PersistError`. An artefact refused at enqueue (`queue_full`,
  `cancelled`, `worker_unavailable`, E §6.2) settles at its refusal; an
  accepted one by its enqueue call + 5 s (credit wait included), and a
  timeout fails every queued artefact at once. Every enqueue call has
  returned when W begins (t_W). **Bound**: with an empty accepted set (no
  artefact accepted, e.g. a build-free `build` with no execution, or every
  artefact refused) W ends at t_W; otherwise, with t_a the last accepted
  enqueue call (t_a < t_W), W ends by max(t_W, t_a + 5 s), so W lasts
  ≤ 5 s [+ 100 ms]. W leaves the persistence thread idle or unavailable.
  Intent during W ends no deadline early. All `persist` and
  `durability` warnings are final at W. Later physical-cleanup facts go to
  `cleanup.log` only (E §5.2),
  never into the report.
- **E Producer closure**: the event channel is closed and `dropped`
  computed (E §9). In one stderr queue critical section (§8) progress is
  closed: every queued progress line is detached and counted as dropped,
  and `progress_dropped` is read; it is final here, before any document
  that carries it exists, and no later writer decision changes it (the
  writer then holds only result lines and at most the one progress line it
  had already taken, which counts as written). Result lines stay accepted
  until the final output is queued. E performs no I/O; it waits only to
  enter the event transport's enqueue-or-drop critical section (E §9) and
  the stderr queue's (§8), whose holders do bounded work and never block
  inside them. E has no policy deadline; fixtures observe E + F ≤
  [100 ms].
- **Cancellation phase** (total, every job kind): the phase recorded in
  `result.cancellation.phase` is the interval in which intent was **first**
  set, ending at the F lock:

  | Intent first set | `phase` | Effect |
  |---|---|---|
  | before V (P, incl. before the first admission) | `primary` | E §6.4 arbitration; verdict per §2.7 |
  | after V, until the end of A (`test` only) | `fact` | verdict frozen; fact per the cutoff |
  | after A, until the end of H (`test` only) | `hooks` | running hook cancelled, later hooks skipped |
  | after the last of V, A, H, until the end of W | `settlement` | settlement still completes (bounded); nothing else changes |
  | after W, until the F lock is taken (phase E included) | `closure` | nothing changes |

  For `build`, `run`, `cmd` and `hook` jobs (no A, H) the interval after V
  belongs to `settlement` and then `closure`. Every phase other than
  `primary` adds the warning `late_cancellation`. Every recorded phase
  selects the cancellation status (§5.2 row 5).
- **F Finalization** *(lock)*: one critical section snapshots
  `result.cancellation` (first intent and its phase), computes
  `result.pre_delivery_exit` (§5.2) and freezes the report. F performs no
  I/O and waits only for the verdict lock, which no holder keeps across a
  blocking call; its bound is E's observation bound above. Intent after F
  is not recorded; a later notification counts only towards force exit
  (E §8.2).
- **S Persistence** of `report.json` (E §6.2): it is enqueued at the start
  of S into the queue position reserved for it at job start, takes no queue
  credits, so it is never `queue_full` or `cancelled`, and has its own
  deadline, that enqueue + 5 s. With the thread unavailable it fails at once
  with `worker_unavailable`. Since W left the thread idle or unavailable,
  the report never queues behind another artefact. Then the terminal slot
  receives `JobFinished` (report id iff the file was seen published).
  **Bound**: S lasts ≤ 5 s [+ 100 ms] with the thread available at the
  start of S, and [100 ms] with it unavailable. **Combined**: t_W →
  `JobFinished` ≤ W + (E + F) + S ≤ 10 s [+ 300 ms], and ≤ 5 s [+ 300 ms]
  with a stalled thread (unavailable by the end of W); a job with an empty
  accepted set and an available thread reaches `JobFinished` ≤ 5 s
  [+ 300 ms] after t_W. Allowances scale per E §10.1; the 5 s deadlines
  never do.
- **D Delivery** (§6, §7), then the final stderr summary (§8; the
  stderr final-drain deadline ends the wait for it 5 s after its
  enqueue instant t_q, not after D), then the process status (§5).

### 2.10 `hook` jobs (`on_switch`)

After `cpf switch` committed its selection, its planned `on_switch` hooks
(C §8.3) run as one job of kind `hook`: admitted like any job (S §6.2),
the hooks are its primary executions in listed order, a failing hook does
not stop later ones, and the phases are P, V, W, E, F, S (no A, H, D:
`switch` has no machine output). Its verdict follows §2.7 and is shown on
stderr. Effect on the status of `cpf switch`, whose selection is already
committed (stderr says so whenever the status is not 0):

| Hook job situation | `cpf switch` status |
|---|---|
| verdict `succeeded` | 0 |
| ordinary hook failure: verdict `wall_deadline`, `output_budget`, `signal`, `nonzero_exit` or `infrastructure` | 0, `hook` warning on stderr (C §8.3) |
| admission failure of class `workspace` or `config` (e.g. `job-seq` unusable, lock `Busy`, retention) | 0, warning on stderr; no hook runs |
| cancellation recorded (any phase, §2.9) or job verdict `cancelled` | cancellation status (§5.3) |
| internal failure: verdict `internal_error`, a caught panic in admission or in the job (class `internal`) | 70 |
| forced cancellation | §5.2 row 2 |

## 3. JSON encoding

Applies to `report.json`, the capture manifest (§4.2), every `--json`
document of every command (including documents other contracts own, C §10)
and every `--events` line.

### 3.1 Canonical writer profile

Writers MUST produce exactly this form; §9.4 compares bytes.

- UTF-8 JSON (RFC 8259), no byte order mark, compact (no insignificant
  whitespace), no duplicate member names.
- A **document** is an object whose first members are `"schema"` and
  `"document"` (format name: `job_report`, `capture_manifest`, `error`,
  `event_stream`, `config_show`, …): it starts with the bytes
  `{"schema":<n>,"document":"`. Event envelopes (§7.3) are exempt.
- Members in the order of the schema tables.
- Strings: `"` and `\` escaped as `\"`, `\\`; U+0000–U+001F as `\n`, `\r`,
  `\t` or `\u00XX` (lowercase hex); nothing else escaped.
- Numbers only in fields typed `u32`, `i32`, `code` (§3.2), minimal decimal:
  no `+`, no leading zero, no `-0`, no fraction, no exponent. No JSON
  floating-point number appears anywhere.
- A stdout document is followed by exactly one `\n`.

### 3.2 Field types

Serialized maxima include quotes; they feed §3.7.

| Type | Wire form | Max bytes |
|---|---|---|
| `u32` | JSON number | 10 |
| `i32`, `code` | JSON number; `code` is an exit status in [−2^31, 2^32 − 1] | 11 |
| `u64` | decimal string, digits only, no leading zero except `"0"` | 22 |
| `i64` | as `u64` with `-` for negative values, never `"-0"` | 23 |
| `dur` | `u64` microseconds, monotonic clock | 22 |
| `bool` / `null` | | 5 / 4 |
| `time` | `"YYYY-MM-DDTHH:MM:SS.mmmZ"`, UTC, system clock, informative | 26 |
| `hex64` / `hex32` | lowercase hex SHA-256 / epoch | 66 / 34 |
| `token` | snake_case `[a-z0-9_]`, 1–32 bytes | 34 |
| `rid` | run id `r` + 5 digits (§3.4) | 8 |
| `jid` | job id (S §6.2) | 36 |
| `ident` | Identifier (S §2.1, ≤ 64) | 66 |
| `pident` | problem identity `local:` + Identifier | 72 |
| `ver` | crate version, ≤ 32 ASCII | 34 |
| `rel` | job- or workspace-relative path built from the ids above; maximum stated per member | per member |
| `text` | cpf-generated text, serialized ≤ 260 bytes including quotes (the writer cuts at a character boundary and ends with `…`); informative | 260 |
| `dec` | a TOML float as `{"text": shortest round-trip, "bits": 16 hex}` (compare spec §6 `F`) | 69 |
| `ns` | full native value (§3.3), n source bytes | max(2n + 11, 4⌈n/3⌉ + 16) |
| `nsum(P)` | bounded native summary (§3.3) | 2P + 149 |
| `bytes` | `{"bytes_b64": …}` of n bytes | 4⌈n/3⌉ + 17 |

Object cost: 2 (braces) + Σ (|key| + 3 + value) + (members − 1) commas.
List of n items: 2 + Σ items + (n − 1).

### 3.3 Native values and bytes

- **Platform.** Every document has the top-level member `platform` (`unix`
  or `windows`); native values are interpreted relative to it. Unix values
  are byte strings; Windows values are WTF-16, carried as WTF-8 bytes.
- **`ns`**: `{"utf8": str}` iff the bytes are valid UTF-8 **and** contain no
  U+0000–U+001F and no U+007F (so only `"` and `\` escape: ≤ 2×);
  otherwise `{"bytes_b64": str}`. One canonical form per value. Used where
  a contract needs the complete value: the capture manifest, C §10 `config
  show`, `spec.json` (E, §10).
- **`nsum(P)`**: `{"head": ns-of-prefix, "len": u64, "truncated": bool,
  "sha256": hex64}`; `head` encodes the first ≤ P bytes (cut back to a
  UTF-8 character boundary when the value is `utf8`-eligible), `len` is the
  full byte length, `sha256` covers the full bytes, `truncated` iff `len`
  exceeds the bytes in `head`. A summary is never presented as the value.
- **`bytes`**: always `{"bytes_b64": …}`, also for valid UTF-8.
- **Base64**: RFC 4648 §4, standard alphabet, `=` padding, no line breaks,
  unused pad bits zero.
- Excerpts inside `diag` use the compare spec §6 escaping; program output
  appears nowhere else except in `bytes` and `nsum` heads.

### 3.4 Wire names and run ids

- Wire names and values in this contract are normative strings,
  independent of Rust identifiers.
- **Run ids** (decision O1): `r` + five decimal digits of the **admission
  index**, which counts admitted executions of the job (builds, test runs,
  command iterations, the raw run, hooks) from `r00000` in admission order,
  allocated once at admission and never reused within the job (≤ 10 000
  planned executions, C §8.2, so five digits suffice). The admission index
  is distinct from the plan index. The run directory is created with
  `create_dir`; an existing entry is never reused or written: that
  execution becomes `not_started{setup}` with the OS error (class
  `infrastructure`), and its run id is consumed. A build's `build_id` is its
  run id (`builds/r00000/`).

### 3.5 Reader acceptance

- Readers MUST reject: invalid UTF-8, duplicate member names, a wrong type,
  an integer outside its field's range, a non-canonical integer string
  (`"01"`, `"+1"`, `"-0"`), a JSON number in a `u64` field, a number with
  fraction or exponent, non-canonical base64, an `ns` in `bytes_b64` form
  whose bytes are `utf8`-eligible, and a missing required member.
- Readers MUST ignore unknown members and MUST NOT depend on member order,
  except that cpf identifies a `report.json` version by its prefix (§3.1,
  ≤ 64 bytes) for pruning (S §6.3); the prefix identifies the version only.

### 3.6 Required, nullable, variant members; lists; versioning

- Every member in a schema table is **required**; a type marked `?` is
  **nullable** (present, `null` when absent).
- A tagged object (`{"kind": …}`) has exactly the members its variant
  lists, after `kind`. An enum whose values all carry no data is a `token`;
  an enum with any data-carrying value is always a tagged object.
- `[T; ≤n]`: a list bounded by its domain (it cannot exceed n). `[T; n]`: a
  capped list, followed immediately by `<name>_dropped: u32` (`0` when
  nothing was dropped); which entries are kept is stated per list.
- Within schema 1 a writer may add members. Removing or renaming a member,
  changing a type or meaning, or adding an enum value is a new schema
  version. Before the first release, schema 1 is still being defined and
  may change in place. A reader meeting a newer `schema` uses only `schema`,
  `document`, `platform` (S §9: raw display only); cpf never prunes a job
  directory whose `report.json` is newer.

### 3.7 Size bounds

Every bound is a sum of serialized maxima (§3.2) over the actual variant
schemas of §4, taking the maximum over mutually exclusive variants and
including escaping and base64. §9.4 serializes the worst case of each item
to check the arithmetic. Sizes in bytes.

| Item | Composition (worst variant) | Sum | Bound |
|---|---|---|---|
| `Cause` | `capture_loss`: kind, stream, loss tokens | 129 | |
| class | `infrastructure`: kind + 8 × 129 + `causes_dropped` | 1 122 | |
| class (not infrastructure) | `signal`: kind, `i32`, name token, bool | 169 | |
| `execution` (summary) | `started`: kind, status 108, trigger 87, `dur`, 3 × `Violation` 119, cleanup 87, `u32` | 804 | |
| stream capture | 3 `u64` + `exceeded_at`, 2 bool, 6 × `Loss` 66, `file` 19 | 608 | |
| `Measurement` | 7 tokens + `u64` | 346 | |
| `InstalledLimit` | 2 tokens, 6 `u64`, `outer` 74 | 360 | |
| **`RunSummary`** | class 1 122 + execution 804 + timing 114 + deadline 22 + 2 × 608 + 5 × 346 + 2 × 360 + 8 tokens + 16 tokens + dropped + `u64` + keys | 6 774 | **7 KiB** |
| `Ref` | digest 66, path 20, `PersistError` 337 (`io`: token, `i32`, text) | 451 | |
| **`executions[]` entry** | index, stage, role 162, coverage 160, run 8, program `nsum(256)` 655, 2 × `Ref`, max(summary 6 774, class 1 122) (mutually exclusive), `class` key with `null` 13, invariant 47 | 8 851 | **9 KiB** |
| **`tests[]` entry** | max of: non-infrastructure class 169 + check with `diag` ≤ 2 048 (2 868); infrastructure class 1 122 + small check (1 723) | 2 868 | **3 KiB** |
| **`aggregates[]` entry** | test 66, 2 × `u32`, three `dur` | 224 | **256** |
| warning | max of: `recovery` (`u32`, `nsum(128)` 399, text); `persist` (token, rid, `PersistError` 337, text) | 741 | **768** |
| hook entry | name 66, trigger, `u32`, outcome (kind + one token) | 244 | |
| capture (header) | check 195 + 2 × (16 × `nsum(128)` + dropped) + counts + `u64` + digest + manifest `rel` 28 | 13 311 | |
| build (header) | max of: `succeeded` + 16 artifacts × 399 (6 574); `failed{missing_output}` with 16 names × 399 and no artifacts (6 664) | 6 664 | |
| **report header** | job 225, workspace `nsum(1024)` 2 191, problem 509, target 1 587, capture 13 311, fingerprints, build 6 664, result 1 784, fact 453, 32 × hook 244, 24 × warning 741, events, timing, persistence 448, scalars, keys | 53 645 | **64 KiB** |
| **report** | header + Σ per planned execution (entry 8 851 + test 2 868 + aggregate 224 + 3 commas = 11 946) | | **64 KiB + 12 KiB × N** |
| `Diagnostic` | file `nsum(256)` 655, key text, 2 × `u32`, message text | 1 237 | |
| **refusal document** | header members a refusal can carry (job, workspace, problem, result, 24 warnings, events, timing, persistence) + refusal (class, error with `nsum(256)`, 64 × 1 237 + dropped) | 103 966 | **104 KiB** |
| **event line** | max of: `run_finished` (summary + 99) 6 873; `output_chunk` (4 096 bytes → 5 464 base64 + 157) 5 621; others ≤ 400 | 6 873 | **7 KiB** (final envelope: report bound + 28, 29 with the line feed) |

N is the number of planned executions (≤ 10 000, C §8.2); the test and
aggregate terms are charged per planned execution because each test has at
least one. The comparator value is budgeted at 8 `dec`-sized members (the
float comparator has 6, compare spec §5.1). The capture manifest has an
exact size computed before it is written (§4.2).

**Reservations** (amendment to E §5.4/§6.2 and S §6.4, §10): before the
first run the job reserves, through the S §6.4 interface, the report bound
(64 KiB + 12 KiB × N) for `report.json` plus the same again for its
temporary file during replacement; the capture manifest's exact size (plus
its temporary) is reserved before it is written during freezing.

## 4. Report schema (`job_report`, schema 1)

### 4.1 Top level

| Member | Type | Content |
|---|---|---|
| `schema` | `u32` | `1` |
| `document` | `token` | `job_report` |
| `platform` | `token` | `unix`, `windows` |
| `outcome` | `token` | `finished` (a ready job ran) or `refused` (§4.12) |
| `cpf_version` | `ver` | |
| `command` | `token` | `build`, `test`, `run`, `cmd`, `hook` |
| `job` | object? | §4.2; `null` only for a refusal before registration |
| `workspace` | `nsum(1024)?` | workspace root as cpf located it |
| `problem` | object? | §4.2 |
| `target` | object? | §4.3 |
| `capture` | object? | §4.2 |
| `fingerprints` | object? | `{source: hex64, tests: hex64}` once run facts compute them (storage contract §6.5); `null` until then |
| `build` | object? | §4.4 |
| `executions` | `[Execution; ≤10000]` | §4.5, plan order |
| `tests` | `[Test; ≤10000]` | §4.6, frozen-manifest order |
| `aggregates` | `[Aggregate; ≤10000]` | §4.7; empty unless `cmd` |
| `result` | object | §4.8 |
| `fact` | object? | §4.9 |
| `hooks` | `[Hook; 32]` | §4.9, plan order, first 32 |
| `warnings` | `[Warning; 24]` | §4.10, first 24 in occurrence order, subject to the late-cancellation reservation |
| `events` | object | `{dropped: u64}` (§7.4; `"0"` without a consumer) |
| `timing` | object | `{started_at: time, finished_at: time, duration_us: dur}`: admission start (S §6.2 step 1) to F |
| `refusal` | object? | §4.12 |
| `persistence` | object | §4.11; stdout document and final event envelope only |

### 4.2 Job identity and capture

- `job`: `{id: jid, dir: rel (≤ 50), sequence: {epoch: hex32, n: u64},
  kind: token}`; `dir` = `.cpforge/runs/<id>`.
- `problem`: `{identity: pident, name: ident, kind: text, dir: rel
  (≤ 75)}`; `kind` as stored, including unknown strings (C §9).
- `capture` (S §6.2 as amended, §7; workspace design §3.2): `{check: Check,
  sources: [nsum(128); 16], support_files: [nsum(128); 16], counts:
  {sources: u32, support_files: u32}, bytes: u64, lists_sha256: hex64,
  manifest: rel (≤ 26)}`.
  - `Check`: `{"kind":"checked"}` or `{"kind":"unchecked","reasons":
    [token; ≤4]}`; schema 1 reason: `shell_stage`.
  - `sources`, `support_files`: **previews**, the first 16 problem-relative
    paths of each role in configured order; `lists_sha256`: SHA-256 over
    the full ordered lists in the workspace design §3.2 encoding (count,
    then each path as a length-prefixed byte string; sources, then support
    files).
  - `manifest`: `inputs/capture.json`, the **capture manifest** (decision
    O2), a document `{"schema":1,"document":"capture_manifest","platform":
    …,"job":jid,"check":Check,"sources":[ns…],"support_files":[ns…],
    "lists_sha256":hex64}` holding the complete lists with full values. It
    is written during freezing as the last capture step (S §6.2, §7), after
    every copy, by no-clobber publication (S §4.3) into the job directory
    under the lease, with its exact size reserved first (§3.7). Publication
    failure (not committed) is a capture failure: the job is refused
    (class `workspace`, exit 8), never ready. `Durability::Uncertain` or
    `Cleanup::Pending` after commit is a `durability` warning. A ready job
    therefore always has a published manifest; the report names it only
    then.
  - `capture` is `null` for a job without a source-consuming stage (`cmd`
    without `needs_build`).

### 4.3 Target and evidence

`target`: `{name: ident?, language: ident?, profile: ident?, build_free:
bool, compare: Comparator?, run_deadline: {us: dur, source: token}?,
published_limits: {time_ms: u32?, memory_bytes: u64?}?, local_limits:
[LocalLimit; ≤2]}`.

- `compare`: `{"kind":"tokens"}`, `{"kind":"exact"}`, `{"kind":"lines"}` or
  `{"kind":"float","abs":dec?,"rel":dec?,"rel_base":token?,"combine":
  token?,"ulps":i64?,"int_exact":bool}` (compare spec §5.1, fully expanded);
  `null` without checks.
- `run_deadline.source`: `configured`, `derived` (C §7.1), `default`, `cli`.
- `published_limits`: informative, never enforced, separate from
  `local_limits` (design §6.1).
- `LocalLimit`: `{kind: token, configured: u64, installed: u64?,
  mechanism: token, outer: {"kind":"none"} | {"kind":"known","bytes":u64}
  | {"kind":"unknown"}}`, kinds `address_space`, `job_commit` (E §7.2).

### 4.4 Build

`build`: `null` for a build-free target or a `cmd` without `needs_build`;
otherwise `{execution: u32, build_id: rid?, output_root: rel? (≤ 15),
result: BuildResult, artifacts: [nsum(128); 16]}`.

- `execution`: plan index of the build. `build_id` and `output_root`
  (`builds/<build_id>`) are `null` iff the build was skipped; after
  `output_too_large` the directory is removed and `output_root` names what
  was removed.
- `BuildResult`: `{"kind":"succeeded"}`, `{"kind":"cancelled"}`,
  `{"kind":"infrastructure","causes":[Cause; 8],"causes_dropped":u32}`,
  `{"kind":"failed","cause":BuildCause}`, `{"kind":"internal_error",
  "invariant":token}`. `BuildCause`: a class object (§4.5) other than
  `infrastructure`/`cancelled`, or `{"kind":"missing_output","names":
  [nsum(128); 16],"names_dropped":u32}`,
  `{"kind":"generated_program_invalid","name":nsum(128),"reason":text}`,
  `{"kind":"output_too_large","bytes":u64,"cap":u64}`.
- `artifacts`: the validated artifact set (first 16) for `succeeded`; empty
  otherwise.

### 4.5 Executions

`executions[]` entry, one per planned execution:

| Member | Type | Content |
|---|---|---|
| `index` | `u32` | plan index |
| `stage` | `token` | `build`, `run`, `command`, `hook` |
| `role` | object | `{"kind":"build"}`, `{"kind":"test","test":ident}`, `{"kind":"command","test":ident?,"iteration":u32}`, `{"kind":"raw"}`, `{"kind":"hook","name":ident,"trigger":token}` |
| `coverage` | object | `{"kind":"started"}`, `{"kind":"transferred"}`, `{"kind":"not_started","reason":NotStarted}`, `{"kind":"skipped","reason":token}` |
| `run` | `rid?` | `null` iff `skipped` |
| `program` | `nsum(256)?` | resolved executable (E §2); `null` iff `skipped` or interrupted preparation (below) |
| `spec` | `Ref?` | `null` iff `skipped` or interrupted preparation (below) |
| `record` | `Ref?` | `null` iff `skipped` or `admitted` |
| `summary` | `RunSummary?` | `null` iff `skipped`, `admitted`, or `invariant = summary_underivable` |
| `class` | object? | only with `summary` `null`: the run class (§2.2, in the `summary.class` form) derived before the summary failed (`invariant: summary_underivable`); `null` in every other case (with a non-null `summary` the class is `summary.class`). Mutually exclusive with `summary`, so the §3.7 entry bound is unchanged |
| `invariant` | `token?` | non-null iff this execution is an orphan (its coverage or record violates a §2.1 legality condition or §2.6 rule) or its evidence is missing or underivable (`record_missing`, `summary_underivable`); coverage, run, record and summary are still the observed values |

- `coverage` also has the variant `{"kind":"admitted"}` (§2.1), which
  always comes with `invariant: "record_missing"`.
- **Creation cancellation**: `transferred` is a finalized cancellation
  snapshot (E §3.4), with class `cancelled`, non-null `run`, `program`,
  `spec`, `record` and `summary`, and no invariant. The job persists it
  normally. Its record and summary execution are `{"kind":"transferred"}`;
  status, creation success and cleanup completion are unknown. Stream
  counters are unobserved (`complete: false`, `file: null`); measurements,
  limits and supervisor errors are empty. `timing.setup_us` is elapsed time
  from admission to the snapshot; total creation duration is unknown.
  `wall_us` and `observer` are null because creation has not returned. The later cleanup ticket outcome
  does not rewrite the record or report. `Cov.transferred` counts this
  coverage separately; it makes a primary population partial.
- **Interrupted preparation**: when cancellation stops run preparation on
  the creation thread (E §3.4) before run-directory creation and recipe
  resolution complete, the execution is
  `not_started{cancelled_before_start}` with `program` and `spec` `null`.
  No other coverage or `not_started` reason allows a `null` `program` or
  `spec`. `run` stays the id allocated at admission (non-null); its
  directory may be absent or incomplete. `record` is non-null as for any
  `not_started`.
- `Ref`: `{digest: hex64?, path: rel? (≤ 18), error: PersistError?}`;
  `spec.digest` is E §2's `spec_digest`, `record.digest` is `null`. After
  phase W exactly one of `path` (job-relative `<run>/spec.json`,
  `<run>/record.json`, seen published) and `error` is non-null.
- `PersistError`: `{"kind":"timeout"}`, `{"kind":"queue_full"}`,
  `{"kind":"cancelled"}`, `{"kind":"worker_unavailable"}`,
  `{"kind":"io","os_error":i32?, "message":text}`. `queue_full`: no queue
  credits within the credit wait limit or budget (E §6.2); `cancelled`: no
  credits, and cancellation intent ended or forbade the wait (E §6.2). Both
  occur only for specs, records and `cleanup.log`, never for the report
  (§2.9 S, §4.11). `cancelled` leaves the §3.7 bounds unchanged (`io` is the
  largest variant).
- `NotStarted`: `{"kind": token}` with the E §6.1 reasons
  `cancelled_before_start`, `create_failed` (+ `api: token`, `os_error:
  i32`), `limit` (+ `os_error`), `unsupported_limit`, `unsupported`,
  `containment_unavailable`, `setup` (+ `os_error`), `cleanup_capacity`,
  `worker`, `retention_budget`, `spec_too_large`.

`RunSummary` (also `run_finished.summary`; full detail in `record.json`):

| Member | Type | Content |
|---|---|---|
| `class` | object | `{"kind":"exited_zero"}`, `{"kind":"nonzero_exit","code":code}`, `{"kind":"signal","signal":i32,"name":token?,"core_dumped":bool}`, `{"kind":"wall_deadline"}`, `{"kind":"output_budget","stream":token,"streams":[token; ≤2]}`, `{"kind":"cancelled"}`, `{"kind":"infrastructure","causes":[Cause; 8],"causes_dropped":u32}` |
| `execution` | object | `{"kind":"not_started"}`, `{"kind":"transferred"}` or `{"kind":"started","status":Status,"stop_trigger":Trigger?,"enforced_at_us":dur?,"violations":[Violation; ≤3],"cleanup":Cleanup,"kill_attempts":u32}` |
| `timing` | object | `{setup_us: dur, wall_us: dur?, observer: token?}` (`pidfd`, `kqueue`, `polling`, `process_handle`; null for `transferred`) |
| `deadline_us` | `dur` | |
| `stdout`, `stderr` | object | `{observed: u64, stored: u64, forwarded: u64, eof: bool, complete: bool, exceeded_at: u64?, losses: [Loss; ≤6], file: rel? (≤ 17)}` |
| `measurements` | `[Measurement; ≤5]` | one per requested kind |
| `limits` | `[InstalledLimit; ≤2]` | `LocalLimit` plus `normalised: u64`, `inherited_soft: u64?`, `inherited_hard: u64?` |
| `supervisor_errors` | `[token; ≤8]` | §2.2 order |
| `notes` | `[token; 16]` | E §6.1 `Note` values and §2.2 notes, first-occurrence order |
| `cwd_overflow_bytes` | `u64?` | S §6.4 |

- `Cause`: `{"kind":"not_started","reason":token}`,
  `{"kind":"status_unknown"}`,
  `{"kind":"cleanup_incomplete","survivors":u32}`,
  `{"kind":"supervisor_error","error":token}`,
  `{"kind":"capture_loss","stream":token,"loss":token}`.
- `Status`: `{"kind":"exited","code":code,"ntstatus":token?}`,
  `{"kind":"signalled","signal":i32,"core_dumped":bool}`,
  `{"kind":"unknown"}`. `Trigger`: `{"kind":"wall_deadline"}`,
  `{"kind":"output_budget","stream":token}`, `{"kind":"cancelled"}`.
  `Violation`: `{"kind":"wall_deadline"}`,
  `{"kind":"output_budget","stream":token,"offset":u64}`.
- `Cleanup`: `{"kind":"confirmed"}`, `{"kind":"incomplete","survivors":u32}`,
  `{"kind":"unverified","reason":token}` (`namespace_unknown`,
  `proc_filtered`, `malformed`, `io`, `scan_incomplete`, `truncated`,
  `info_failed`, `unsupported`, `query_failed`, `root_not_observed`,
  `observer_failed`).
- `Loss`: `{"kind":"budget_discard"}`, `{"kind":"write_error",
  "os_error":i32}`, `{"kind":"write_stalled"}`,
  `{"kind":"no_eof_before_drain_deadline"}`,
  `{"kind":"pipe_read","os_error":i32}`, `{"kind":"forward_incomplete"}`.
- A started stream with no capture file (including sink `Discard`) has
  `complete: false` and `file: null`; zero counters do not assert that no
  bytes were produced.
- `file`: job-relative capture path (`<run>/out/stdout`), set only when the
  capture file was published.
- `Measurement` (E §7.1): `{kind, value: u64?, unit, scope, aggregation,
  completeness, method, reason: token?}`, all others `token`: kinds
  `peak_rss`, `peak_working_set`, `peak_job_commit`, `cpu_time`,
  `process_lifetime`; units `bytes`, `microseconds`; scopes `root`,
  `root_and_waited_descendants`, `job`; aggregations `max_of_processes`,
  `sum`, `job_peak`; completeness `final`, `partial`; `method` a method
  token (`wait4_ru_maxrss`, …); `reason` a code (`unsupported_platform`,
  `not_validated`, `cleanup_not_confirmed`, `accounting_failed`), the full
  text staying in `record.json`. `value = null` and `completeness =
  partial` each require a non-null `reason`. Every metric in the spec's
  `measure` appears; unrequested metrics are absent.

### 4.6 Tests

`tests[]` entry (test jobs and `cmd` with `for_each_test`):

| Member | Type | Content |
|---|---|---|
| `id` | `ident` | test id (S §8.1) |
| `origin` | `token` | `sample`, `custom`, `stress`, `imported` |
| `answer` | `token` | `absent`, `empty`, `present` |
| `input_sha256` | `hex64` | of the frozen copy (S §7) |
| `answer_sha256` | `hex64?` | `null` iff `absent` |
| `executions` | object | `{first: u32, count: u32}`: contiguous plan-index range (test-major order, C §8.2) |
| `class` | object? | the test's run class (`test` jobs); `null` without a record or for `cmd` |
| `check` | object? | `null` for `cmd`; else §2.4: `{"kind":"passed"}`, `{"kind":"failed","fail":"wrong_answer","reason":token,"diag":Diag}`, `{"kind":"unchecked","reason":"no_answer"}`, `{"kind":"checker_failed","class":"test_data_error","reason":"invalid_answer","diag":Diag}`, `{"kind":"check_error","os_error":i32?}`, `{"kind":"not_admitted","reason":token}` (`run_not_eligible`, `cancelled`), `{"kind":"internal_error","invariant":token}` (underivable, §2) |
| `diagnostic` | `bool` | the check was admitted after an unsuccessful class |
| `verdict` | object? | `null` for `cmd`; `{"kind":<§2.5 token>}`, `{"kind":"not_run","reason":token}`, `{"kind":"internal_error","invariant":token}` |

`Diag` reason tokens: `token_mismatch`, `line_mismatch`, `byte_mismatch`,
`count_mismatch`, `class_mismatch`, `integer_mismatch`, `tolerance`,
`ulp_excess`, `out_of_range`, `incomplete_output`, `invalid_answer`; `Diag`
is the compare spec §6 wire object (≤ 2 KiB there).

### 4.7 `cmd` aggregates

Per test (one entry with `test: null` without `for_each_test`), C §8.2:
`{test: ident?, iterations: u32, succeeded: u32, wall_us: {min: dur,
median: dur, max: dur}?}` over successful iterations; median of an even
count is the lower middle; `null` when none succeeded.

### 4.8 Result

`result`: `{verdict: token, primary_verdict: token, partial: bool,
cancellation: {requested_at_us: dur, source: token, phase: token}?,
pre_delivery_exit: {code: code, reraise: token?}, counts: Counts,
internal: {invariant: token, locus: Locus, message: text}?}`.

- `verdict`, `primary_verdict`: §2.7 (`refused` for §4.12).
- `cancellation`: snapshot at F; source `console` (Ctrl-C, `SIGINT`,
  Windows console events), `terminate` (`SIGTERM`), `api`; phase
  `primary`, `fact`, `hooks`, `settlement`, `closure` (§2.9).
- `pre_delivery_exit`: the status §5.2 selects at F; `reraise` is `sigint`
  or `sigterm` when §5.3 will terminate by re-raising, else `null`. The
  process status differs from it only by a later delivery failure (row 4),
  force exit (row 2) or an internal error caught after F (row 3).
- `Counts`: `{primary: Cov, hooks: Cov, tests: u32, by_verdict: ByVerdict,
  by_class: ByClass}`. `Cov` = `{planned, started, not_started, transferred, skipped,
  admitted, invalid}` (`u32`); `planned = started + not_started + transferred + skipped +
  admitted` counts observed coverage (§2.1), and `invalid` (not part of that sum) counts the
  executions of the population with a non-null `invariant`. `ByVerdict` has a `u32` member for every §2.5
  token (`passed`, `wrong_answer`, `unchecked`, `checker_failed`,
  `wall_deadline`, `output_budget`, `signal`, `nonzero_exit`, `cancelled`,
  `infrastructure`, `not_run`, `internal_error`), `0` when absent, all zero
  without tests. `ByClass` has a `u32` member for each §2.2 class token and
  `skipped` and `unknown`, over primary executions, counting observed
  classes (an orphan execution with a record still counts under its
  observed class; its invalidity is counted only in `invalid`); an
  `admitted` execution, and one with `invariant: summary_underivable` whose
  class was not derived, has no class and counts under `unknown`; one whose
  class was derived counts under that class; both count in `invalid`. Test
  verdicts derived from missing evidence count under
  `by_verdict.internal_error`.
- `internal`: set iff an internal failure was recorded before F (§2), the
  first one; `Locus`: `{"kind":"job"}`, `{"kind":"build"}`,
  `{"kind":"execution","index":u32}`, `{"kind":"test","test":ident}`,
  `{"kind":"phase","phase":token}` (a panic outside any execution, e.g. in
  fact publication or settlement).

### 4.9 Fact and hooks

- `fact`: `null` for jobs other than `test`; else `{outcome: Outcome,
  warnings: [token; ≤2]}` (`durability_uncertain`, `cleanup_pending`, also
  reported as `durability` warnings). `Outcome`: `{"kind":"published"}`,
  `{"kind":"superseded"}`, `{"kind":"not_published","reason":token,
  "message":text?}` (S §6.5: `sequence_unavailable`, `epoch_changed`,
  `fact_too_new`, `fact_unreadable`, `lock_busy`, `io`),
  `{"kind":"not_attempted","reason":token}` (`cancelled`,
  `not_completed`). `published` stays `published` whatever happens later.
- `hooks[]`: `{name: ident, trigger: token, execution: u32?, outcome:
  Outcome}`; `Outcome`: `{"kind":"succeeded"}`, `{"kind":"failed","class":
  token}` (the class token; the full class is in the execution's summary),
  `{"kind":"cancelled"}`, `{"kind":"skipped","reason":token}`,
  `{"kind":"not_planned","reason":token}` (`untrusted`, `disabled`,
  `config_override`, `depth`), `{"kind":"internal_error","invariant":token}`
  (the hook execution is an orphan, its record is missing, or its outcome
  cannot be derived; an internal failure, §2.6).

### 4.10 Warnings

`{kind: token, …, message: text}` (≤ 768 bytes, §3.7). Warnings never
change a verdict or status.

When late cancellation is recorded, reserve one of the 24 slots for
`late_cancellation`, emit it after the first 23 ordinary warnings in
occurrence order, and count every omitted ordinary warning in
`warnings_dropped`; otherwise keep the first 24 ordinary warnings.

| `kind` | Extra members | Source |
|---|---|---|
| `prune` | `job: jid?`, `detail: token` (`rename_failed`, `newer_schema`, `lease_unreadable`, `unmeasurable`, `measurement_incomplete`, `reclaim_leftover`, `reclaim_name_taken`) | S §6.3 at admission |
| `recovery` | `paths: u32`, `first: nsum(128)?` | S §4.5 |
| `persist` | `artefact: token` (`spec`, `record`, `cleanup_log`), `run: rid`, `error: PersistError` | E §6.2, final at W |
| `durability` | `artefact: token`, `detail: token` (`uncertain`, `cleanup_pending`) | S §4.1 |
| `retention` | `detail: token` (`cwd_overflow`, `workspace_over_target`), `run: rid?`, `bytes: u64` | S §6.4 |
| `cleanup_unverified` | `runs: u32` | E §4.4 |
| `fact` | `outcome` (§4.9 fact outcome) | S §6.5 |
| `hook` | `name: ident`, `outcome` (§4.9 hook outcome) | C §8.3 |
| `hooks_disabled` | `reason: token` | C §8.3 |
| `late_cancellation` | `phase: token` | §2.9 |
| `progress_dropped` | `lines: u32` | §8, final at E |

### 4.11 `persistence`

The stdout document and the final event envelope add, after phase S:
`persistence`: `{report: Report, durability: token?}`; `Report`:
`{"kind":"published","path":rel (≤ 60)}`, `{"kind":"failed","error":
PersistError}`, `{"kind":"not_attempted","reason":"refusal"}`;
`durability` (`ok`, `uncertain`) is `null` unless `published`. A report
`error` is never `queue_full` or `cancelled` (§2.9 S).
`report.json` is the same document without `persistence`. A report
persistence failure is a stderr warning and this member, never a status
change (E §9 as amended).

### 4.12 Refusal documents

A refusal writes, in machine mode, a `job_report` with `outcome:
"refused"`, `result.verdict = primary_verdict = "refused"`,
`result.pre_delivery_exit` its status, `persistence.report.kind =
"not_attempted"`, and `refusal`: `{class: token, error: {kind: token,
message: text, path: nsum(256)?}, diagnostics: [Diagnostic; 64]}` (class
`config`, `workspace`, `cancelled`, `infrastructure`, `internal`;
`infrastructure` covers a job that cannot start on this platform or host,
e.g. no supervisor backend, exit 10); `Diagnostic` (C §3.2):
`{file: nsum(256)?, key: text?, line: u32?, col: u32?, message: text}`, in
reporting order.

- `job` is set iff the job was registered (S §6.2 steps 1–5) and freezing,
  capture or manifest publication failed. A refusal is never persisted.
- Members that need a ready job are `null` or empty lists; `warnings` may
  hold the admission's prune and recovery warnings.
- Non-job commands that fail in machine mode write `{"schema":1,
  "document":"error","platform":…,"command":token,"class":token,
  "exit":code,"error":…,"diagnostics":[…],"diagnostics_dropped":u32}`.

### 4.13 `report.json`

Location S §6.1; written once per finished job by temp + rename (E §6.2)
within the §3.7 reservation; never rewritten; classified by S §9
(tool-only). cpf reads only its prefix in the CLI foundation.

## 5. Exit statuses

### 5.1 Codes (decision D1)

| Code | Name | Meaning |
|---|---|---|
| 0 | `success` | the command did what was asked; jobs: `passed`, `succeeded` |
| 1 | `failed` | the solution or a program failed: `wrong_answer`, `wall_deadline`, `output_budget`, `signal`, `nonzero_exit` |
| 2 | `usage` | invalid arguments (clap) or a rejected combination: `--json`/`--events` with raw `cpf run`, `--json` with `--events`, `--problem` with `switch`, both test sources on stdin; *(organisation, draft)* an organisation argument outside its grammar (tag, status name, filter value, search term), `--problem` with `tag`/`untag`/`mark`, `mark` or `notes` on `contest:<name>` (S §5.9) |
| 3 | `build_failed` | the build failed |
| 4 | `not_verified` | nothing failed but not everything was checked: `unchecked`, `no_tests` |
| 5 | `checker_failed` | judging could not decide (schema 1: invalid answer) |
| 6 | `cancelled` | cancellation (API, Windows, or Unix when not re-raised, §5.3) |
| 7 | `config` | failure class `config`: C §3 phases 1–3, unsupported problem kind (C §9), missing context (C §8.1), accident guard (S §7); *(organisation, draft)* a workflow status outside the vocabulary, or an undeterminable `status.values` (S §5.9.4, C §7.5) |
| 8 | `workspace` | failure class `workspace`: no workspace, selection errors (S §5.7, `NoSelection`), record classification failures, unknown or unhealthy test, lock `Busy`, admission refused, freeze, capture or manifest failures, storage I/O |
| 9 | `partial` | failure class `partial`: a metadata command committed part of its work (S §5.2 step 5, S §5.3 step 4, `clean --apply`) |
| 10 | `infrastructure` | cpf could not run or observe a program reliably |
| 11 | `delivery` | requested machine output could not be written (§6.3, §7.6) |
| 70 | `internal` | failure class `internal`: a bug detected and handled (§5.4) |

12–63 are reserved for later features; of 64–78 (`sysexits`) only 70 is
used; 126, 127 and ≥ 128 are never chosen as exit codes (the Unix re-raise
of §5.3 is a death by signal).

### 5.2 Precedence

For every command, job or not, the status is decided by the **first** true
condition:

| # | Condition | Status |
|---|---|---|
| 1 | failure class `usage` (nothing started) | 2 |
| 2 | **forced** cancellation (E §8.2 force exit; delivery abandoned) | §5.3 |
| 3 | failure class `internal` (any time, including after F), or job verdict `internal_error` | 70 |
| 4 | machine output requested and its delivery failed (decision D9) | 11 |
| 5 | orderly cancellation: `result.cancellation` set at F, failure class `cancelled`, job verdict `cancelled`, or cancellation of a `hook` job (§2.10) | §5.3 |
| 6 | failure class `config` | 7 |
| 7 | failure class `workspace` | 8 |
| 8 | failure class `partial` | 9 |
| 9 | job verdict `infrastructure` | 10 |
| 10 | job verdict `build_failed` | 3 |
| 11 | job verdict `checker_failed` | 5 |
| 12 | job verdict `wall_deadline`, `output_budget`, `signal`, `nonzero_exit`, `wrong_answer` | 1 |
| 13 | job verdict `unchecked`, `no_tests` | 4 |
| 14 | otherwise | 0 |

- Rows 6–8 apply to job refusals and to non-job command failures alike
  (`cpf current` with `NoSelection` → class `workspace` → 8; `cpf config
  show` on invalid configuration → class `config` → 7).
- Pairs: forced cancellation + anything → row 2; internal + delivery
  failure → 70; delivery failure + orderly cancellation → 11; delivery
  failure + any verdict other than `internal_error` → 11; refusal +
  delivery failure → 11; `infrastructure` + `wrong_answer` → 10 (via the
  verdict).
- `pre_delivery_exit` is rows 3 and 5–14 evaluated at F.
- For `cpf switch`, the `hook` job selects a row only through row 3
  (verdict `internal_error` or a caught panic, class `internal`) or row 5
  (cancellation); its ordinary verdicts and its admission failures of class
  `workspace` or `config` select none (§2.10). Warnings, persistence
  failures, fact outcomes, **ordinary** `after_test` hook outcomes and
  stderr write failures never select a row; an `internal_error` hook
  outcome selects row 3.
- A truncated or missing stdout document can accompany 2, 70, row 2's
  status or a crash, not only 11: consumers MUST validate the document
  (complete line, parses, `schema`).

### 5.3 Cancellation status (decision D3)

- **Unix, signal-origin cancellation** (`console` from `SIGINT`,
  `terminate` from `SIGTERM`), rows 2 and 5: after orderly cleanup and, for
  row 5, after delivery and the stderr summary, cpf restores the signal's
  default disposition, unblocks it and sends it to itself, so the parent
  sees death by `SIGINT` (shells: 130) or `SIGTERM` (143). For force exit
  the signal is the one that forced it. If cpf survives the re-raise, it
  exits 6.
- **API cancellation** and **Windows** (Ctrl-C, Ctrl-Break, console close):
  6.
- A delivery failure suppresses the re-raise (row 4 precedes row 5): 11.

### 5.4 Process-level rules

- Unix start-up: before opening any file, each closed descriptor 0–2 is
  opened to `/dev/null`; a closed stdout is remembered (§6.3).
- **Panics** (best effort): the release profile keeps `panic = "unwind"`;
  the top level runs the command under `catch_unwind` and job threads hand a
  caught panic to the job at join. A caught panic is failure class
  `internal` (70) in every case:
  - **before the job is ready** (or in a non-job command): before any byte
    of machine output, a refusal document of class `internal` (job
    commands) or an `error` document (non-job commands) is written;
  - **in a ready job before F**: an internal failure per §2 (before V or
    after V); the job continues through F, S and D if the report writer is
    intact, and the delivered report says `internal_error`;
  - **after F** (phases S and D, the report frozen): **no document is
    written** that was not already being written. A panic before the first
    stdout byte leaves stdout empty (no error document, since a second
    document would contradict the frozen report); one during delivery
    stops it, leaving a truncated document or stream. `report.json` is
    whatever phase S had completed. The reason goes to stderr if possible.
  **Not covered** (status unspecified): aborts (a panic while panicking,
  `abort`, allocation failure), stack overflow, signals that kill cpf, and
  termination by dependencies.
- **Force exit** (E §8.2): no further **intentional** write to stdout or
  stderr begins after it starts; a write in progress on another thread may
  complete or be cut. Status per §5.3.

### 5.5 Per command

| Command | Possible statuses (besides 2, row 2, 70) |
|---|---|
| `cpf build` | 0, 3, 7, 8, 10, 11, cancellation |
| `cpf test` | 0, 1, 3, 4, 5, 7, 8, 10, 11, cancellation |
| `cpf run` (raw, decision D2) | 0, 1, 3, 7, 8, 10, cancellation |
| `cpf cmd <name>` | 0, 1, 3 (`needs_build`), 7, 8, 10, 11, cancellation |
| `cpf init` | 0, 8 |
| `cpf add` | 0, 7 (template, language), 8, 9 |
| `cpf current` | 0, 8 |
| `cpf switch` | 0, 8, cancellation (during its `hook` job) |
| `cpf test add` | 0, 8, 9 |
| `cpf test register`, `enable`, `disable` | 0, 8 |
| `cpf test ls` | 0; 8 when the catalog is not `missing` or `valid` (listing still printed) |
| `cpf config show` | 0, 7, 11 (`--json`, C §10) |
| `cpf config check` | 0, 7 (no `--json` in schema 1) |
| `cpf config paths` | 0 |
| `cpf doctor` | 0, 7; an unsupported capability is never an error |
| *(draft)* `cpf status` | 0, 8, 11 (`--json`); *(organisation, draft)* filters add only 2 for a filter value outside its grammar |
| *(draft)* `cpf clean` | 0, 8; `--apply`: 9 when a selected candidate could not be renamed or deleted (held leases are skipped) |
| *(draft)* `cpf fsck` | 0, 8 (findings are reported with 0) |
| *(draft)* `cpf adopt`, `ignore`, `gitignore` | 0, 8 |
| *(organisation, draft)* `cpf tag`, `cpf untag` | 0, 8 |
| *(organisation, draft)* `cpf mark` | 0, 7, 8 |
| *(organisation, draft)* `cpf notes` | 0, 8, 11 (`--json`); `--path`: 0, 8, 11 |
| *(organisation, draft)* `cpf ls` | 0 (also with invalid entries and findings), 8, 11 (`--json`) |
| *(organisation, draft)* `cpf search` | 0 (hits or none, complete or incomplete), 8, 11 (`--json`), cancellation |
| `cpf --help`, `--version` | 0 |

"Cancellation" is the §5.3 status. Raw `cpf run` never passes the child's
code through; the child's status is in the report and the stderr summary
(D2). `cpf check` (design §5.4) gets its row with its feature.

## 6. `--json`

### 6.1 Availability

- Accepted by `cpf build`, `cpf test`, `cpf cmd`, and by non-job commands
  whose contract defines a document (schema 1: `cpf config show`, *(draft)*
  `cpf status` below, *(organisation, draft)* `cpf ls`, `cpf search`,
  `cpf notes` below; later `cpf test ls`). A per-command flag; elsewhere
  clap rejects it (2): *(organisation, draft)* `cpf tag`, `untag` and
  `mark` have no document.
- Raw `cpf run` rejects `--json` and `--events` (E §8.1): 2.
- `--json` with `--events`: 2 (decision D4).

*(draft)* **`status` document** (workspace design §3.2, §3.3), schema 1,
§3 encoding: `{"schema":1,"document":"status","platform":…,"problems":
[Problem]}`, one entry per selected problem in selection order. It is not
persisted, and its lists are workspace-sized, not capped (as for
`config_show`). A selection or workspace failure writes the §4.12 `error`
document (`command: "status"`) instead.

| Member | Type | Meaning |
|---|---|---|
| `problem` | `pident` | the problem's identity |
| `tags` | `[string]` | `problem.toml` `[problem] tags`, as written (explicit user status, never derived) |
| `user_status` | `string?` | `problem.toml` `[problem] status` |
| `missing_registered` | `[ns]` | registered paths (S §8.1 catalog entries, the target's `sources`, `support_files`, `solution_files`) that are absent, sorted by path |
| `untracked` | `[ns]` | untracked files (workspace design §2.2), sorted by path |
| `targets` | `[Target; ≤1]` | the `solution` target |
| `diagnostics` | `[text]` | reasons for an `unknown` comparison and other findings, informative |

Paths are problem-relative, native components joined by `/` on every
platform. `Target`:

| Member | Type | Meaning |
|---|---|---|
| `target` | `ident` | `solution` |
| `fact_state` | token | S §9 record classification of the fact: `missing`, `unreadable`, `malformed`, `too_new`, `valid` |
| `comparison` | token | `unchanged`, `source_changed`, `tests_changed`, `source_and_tests_changed`, or `unknown` (fact not `valid`, target or current inputs not fingerprintable) |
| `last_verdict` | token? | the fact's verdict summary iff `fact_state` is `valid` |
| `finished_at` | `time?` | the fact's `finished_at` iff `valid` |
| `capture_check` | `Check?` (§4.2) | the fact's capture check iff `valid` |

Fixture: `src/report/wire/fixtures/status.json`.

*(organisation, draft)* **Organisation fields in `status`.** `tags` and
`user_status` keep their existing meaning unchanged: `tags` holds the
string elements of `[problem] tags` as stored, in order (non-string
elements skipped; `[]` when the key is absent or not an array), and
`user_status` the stored `[problem] status` when it is a string, any
string, otherwise `null`. They are **raw** values, not validated. A new
member, placed after `diagnostics`, reports validity separately:

| Member | Type | Meaning |
|---|---|---|
| `findings` | `[OrgFinding]` | every organisation field (S §5.9.2) that is `invalid`, with its reason; `[]` when all are absent or valid |

Filters (S §5.9.6) use the validated and effective values, never the raw
members, and change only which problems are listed; the run-fact
comparison and every other member are computed exactly as before. The
human form labels the stored status `workflow: <value>` (S §5.9). Fixture
`status.json` gains `"findings":[]`; a second fixture
`status_invalid_fields.json` holds `status = "Working on it"` and `tags =
["dp", "DP"]`: `user_status` `"Working on it"`, `tags` `["dp","DP"]`, and
two findings.

*(organisation, draft)* **Common types.** `OrgFinding`: `{field: token,
reason: text}`, `field` one of `tags`, `status`, `created_at`,
`updated_at`, `solved_at`, `archived_at`. `Source`: `{path: ns, kind:
token, field: token?, message: text}` with `path` workspace-relative
(native components joined by `/`) and `kind` one of `link`,
`not_a_directory`, `invalid_name`, `name_collision`, `metadata_missing`,
`metadata_unreadable`, `metadata_malformed`, `metadata_too_new`,
`identity_mismatch`, `contest_unreadable`, `contest_malformed`,
`contest_too_new`, `invalid_field` (then `field` is set), and in `search`
also `notes_not_regular`, `notes_unreadable`, `notes_not_utf8`. `origin`:
token `active` (workspace) or `snapshot` (archive contract; its members
arrive with it). Timestamps are `time?`: the field when `valid`, a
date-only value normalised to `T00:00:00.000Z`, otherwise `null`. These
documents are not persisted, their lists are workspace-sized and uncapped
except where a cap is stated; a selection or workspace failure writes the
§4.12 `error` document with `command` `ls`, `search` or `notes`.

*(organisation, draft)* **`ls` document**: `{"schema":1,"document":"ls",
"platform":…,"kind":token,"problems":[LsProblem],"contests":[LsContest],
"invalid":[Source]}`. `kind` is `problems` (`contests` empty) or
`contests` (`problems` empty), in S §5.9.7 order.

| `LsProblem` member | Type | Meaning |
|---|---|---|
| `problem` | `pident` | identity (`pident` as widened for remote identities by the fetch amendment) |
| `origin` | token | `active` |
| `title` | `string?` | `[problem] title` when present |
| `tags` | `[string]` | the `tags` field when `valid`, else `[]` (validated, unlike `status`) |
| `user_status` | `string?` | the stored `status` when `valid`, else `null` |
| `effective_status` | `string?` | S §5.9.2: `new` when absent, `null` when invalid |
| `created_at`, `updated_at`, `solved_at`, `archived_at` | `time?` | each field |
| `contests` | `[ident]` | contests listing it, by name |
| `findings` | `[OrgFinding]` | invalid fields, in the member order above |

| `LsContest` member | Type | Meaning |
|---|---|---|
| `contest` | `ident` | directory name |
| `identity` | `string?` | `[contest] id` when present |
| `tags` | `[string]` | top-level `tags` when valid |
| `members` | `u32` | `[[problem]]` record count |
| `counts` | `[{status: string, count: u32}]` | derived contest status: non-zero counts per effective status, built-in names in vocabulary order, then others in byte order |
| `unresolved`, `invalid` | `u32` | members not resolvable; members with an invalid status |
| `findings` | `[OrgFinding]` | an invalid `tags` |

*(organisation, draft)* **`search` document**: `{"schema":1,"document":
"search","platform":…,"terms":[string; ≤16],"notes":bool,"hits":[Hit],
"findings":[Source],"incomplete":bool,"incomplete_reasons":[token; ≤5],
"entries":u32,"bytes_read":u64}`. `notes` is false with `--no-notes`;
`incomplete` is true iff `incomplete_reasons` is non-empty (S §5.9.8
reasons, in that order); `entries` and `bytes_read` are the counters E
and B at the end. `Hit`: `{problem: pident, origin: token, fields: [token;
≤6], notes_lines: [u32; 16], notes_lines_dropped: u32, notes_truncated:
bool}`; `fields` lists the matched fields in the order `identity`,
`title`, `url`, `tags`, `status`, `notes`; `notes_lines` keeps the first 16
matching lines. Hits are bounded by E.

*(organisation, draft)* **`notes` document**: `{"schema":1,"document":
"notes","platform":…,"problem":pident,"path":ns,"state":token}`; `path` is
the absolute native path of `notes.md` (Windows without the verbatim
prefix, S §5.7); `state` is `created` or `existing` (mutating form), or
`absent`, `existing`, `not_regular` (`--path`).

Fixtures: `status_invalid_fields.json` (above), `src/report/wire/fixtures/ls.json`, `ls_contests.json`,
`search.json`, `search_incomplete.json`, `notes.json` (encoder vectors and
the Python oracle round trip, as for `status`).

### 6.2 Output

- stdout carries exactly one document and its `\n`, written in phase D,
  even if persistence failed (E §9). Nothing else is written to stdout by
  cpf, and no child writes to it (job sinks are files or discard, E §2).
- A refusal or non-job failure writes its refusal or `error` document.
  Argument-parsing errors write nothing to stdout.
- Human output goes to stderr (§8); `--color` affects stderr only.

### 6.3 Delivery

- Blocking writes, then flush; delivery succeeded iff every write and the
  flush succeeded. Not fsynced.
- Any write or flush error is a delivery failure (`EPIPE`, `EIO`,
  `ENOSPC`, `EBADF`, …; Windows `ERROR_NO_DATA`, `ERROR_BROKEN_PIPE`, …);
  cpf ignores `SIGPIPE` (checkers design §6.5). After a failure nothing more
  is written to stdout. `report.json` is unaffected (S precedes D).
- Closed stdout at start (§5.4; Windows `GetStdHandle` without a handle)
  with `--json`: nothing is admitted, the reason goes to stderr, 11.
- A stalled stdout blocks phase D (E §9); force exit stays available.

## 7. `--events`

### 7.1 Destination and modes (decisions D4, D10)

- `--events` writes to **stdout** (purity as §6.2).
- `--events` carries job, run and progress events without program output;
  `--events=output` also enables `output_chunk` events for every run of the
  job (E §2 `StreamPolicy.events = true` on both streams). Chunks never
  affect capture, budgets or verdicts.

### 7.2 Framing

- Each line is one compact JSON object terminated by `\n`.
- **Line 1, the stream header**, is a document: `{"schema":1,"document":
  "event_stream","platform":token,"command":token,"cpf_version":ver,
  "output":bool}`; its `schema` versions the stream and its envelopes.
- **Event envelopes** (§7.3) follow, then the **final envelope**
  `{"event":"report","report":<stdout document>}`, then end of stream.
  Envelopes are exempt from the document-prefix rule; the embedded report
  keeps its own `schema`.
- A refusal writes the header and the final envelope with the refusal
  document.

### 7.3 Event envelopes

Each job event envelope starts with `"event"` (token) and `"seq"` (`u64`):

| `event` | Members after `event`, `seq` |
|---|---|
| `job_started` | `job: jid, kind: token, sequence: {epoch, n}` |
| `run_started` | `run: rid, execution: u32, stage: token, role: Role, digest: hex64` |
| `output_chunk` | `run: rid, stream: token, bytes: bytes` (≤ 4096 raw bytes; `=output` only) |
| `run_finished` | `run: rid, summary: RunSummary` |
| `progress` | `done: u32, total: u32` |
| `job_finished` | `job: jid, verdict: token, report: {path: rel}?, dropped: u64` |

### 7.4 Sequence and drops

- `seq` starts at `0` (`job_started`). Assignment is serialized with the
  enqueue-or-drop decision: one critical section per emission attempt takes
  the next number and either enqueues within the transport budget (E §9,
  8 MiB) or counts a drop. Enqueue order equals `seq` order; each number is
  consumed by exactly one attempt.
- Only non-terminal events are dropped. `job_finished` takes the next number
  after the channel is closed (phase E), so its `seq` equals the number of
  earlier attempts.
- `dropped` counts budget drops only. **Gap equation**, valid only for a
  stream whose consumer stayed connected from header to final envelope: the
  number of `seq` values in `[0, job_finished.seq)` missing from the stream
  equals `dropped`. After a delivery failure (§7.6) emission is a no-op that
  still consumes numbers and is not a drop.
- A slow consumer causes drops, never a slower job; the job waits for the
  writer only in phase D.

### 7.5 Terminal barrier

The writer drains the channel until it is closed, then waits for the
terminal slot, which is filled after phase S (E §9 as amended); it writes
`job_finished`, the final envelope, and flushes. A stream ending earlier
was cut: delivery failure, force exit, an internal error caught after
output began (§5.4), or an uncovered crash.

### 7.6 Delivery failure

A write error (as §6.3) disconnects the consumer; the job continues to its
normal end and persists its report; status 11 (§5.2 row 4). Closed stdout
at start: as §6.3.

## 8. Human output (decisions D5, O3)

- Job commands write progress, per-test lines (label + qualifier, §2.8),
  the summary and warnings to **stderr** in every mode; stdout is reserved
  for machine output.
- **One ordered stream.** One stderr writer thread per invocation writes
  every human line of the invocation from one queue, in offer order. Each
  line is rendered, sanitized (control characters become spaces) and cut to
  ≤ 512 bytes including its line feed (ending with `…`) before it is
  offered. Two classes share the queue:
  - **Result lines**: per-test lines, the start-up configuration
    diagnostics, the final warnings and summary, and the final aggregates of
    a stress job. Each takes the next number of one sequence starting at 0
    in the critical section that queues it; the writer writes them in that
    order without gaps. The queue never drops a result line.
  - **Progress lines**: every other line (execution counts, build progress,
    stress iteration updates). Lossy.
- **Result ceiling**, charged in encoded bytes and fixed when the plan is
  final: 512 B × planned tests plus a **final allowance** of 128 KiB
  (131 072 B) that every job has, including one with no planned execution
  (a build-free `build`, §2.7). Planned tests ≤ planned executions ≤ 10 000
  (C §8.2), so the per-test part is ≤ 10 000 × 512 B = 5 120 000 B and the
  whole ceiling ≤ 5 251 072 B (≈ 5.01 MiB). Each planned test offers at
  most one per-test line. The allowance holds ≤ 64 configuration
  diagnostics (further ones are replaced by one line counting them), ≤ 29
  warning lines (24 warnings, one `warnings_dropped` line, §4.10, and ≤ 4
  report-persistence, durability or delivery lines), ≤ 16 summary lines and
  ≤ 128 stress aggregate lines: 237 lines of ≤ 512 B = 121 344 B ≤
  131 072 B. The ceiling is a proven bound, not an admission test: by this
  composition it is never exceeded (debug builds assert it). Before the
  plan is final only the allowance applies. The ceiling bounds memory; it
  is not preallocated.
- **Progress budget**: 64 KiB; each queued progress line is charged
  max(encoded length, 64 B), so ≤ 1 024 are queued. Queued progress lines
  are held apart from queued result lines, each tagged with the number of
  result lines offered before it, so the writer can tell offer order and
  phase E can detach all of them at once. Before E a progress line is
  dropped and counted when its charge does not fit, and when the writer
  reaches it while a result line offered after it is queued (skipped, never
  written later). At E (§2.9) every still-queued progress line is detached
  and counted, and the count becomes final; a progress line offered after E
  is ignored without counting, and the writer meets no queued progress line
  after E. A progress line the writer took before E counts as written,
  whatever its write does. Progress therefore never delays or reorders a
  result line. `progress_dropped` (§4.10) counts progress lines only and
  is final at E.
- **Queue lock**: a plain blocking lock (no try-lock path; contention never
  drops a line). The contract bounds the **work inside** the lock, not the
  time to acquire it: a holder only reads and sets flags and counters,
  assigns one sequence number, and pushes one prepared line, pops one line,
  or swaps in one prepared value (the final batch, or at E an empty
  progress holding for the detached one). Rendering, sanitizing, cutting,
  allocation, freeing lines, condition-variable notification and all I/O
  happen outside it, and no holder blocks or waits while holding it; entry
  storage for the maximum entry count (planned tests + 256 result entries,
  two progress holdings of 1 024 entries) is allocated outside the lock
  before the first offer that could need it. The lock has no fairness
  rule, so acquisition time is not bounded by this contract: a holder that
  the OS deschedules, or another thread that reacquires the lock first,
  delays an acquirer, including the executor thread that also drives
  supervision timers (runtime decision record), as any short critical
  section does. Supervision deadlines are measured on the monotonic clock
  (E §6.3), so such a delay shows up as observation lateness (E §6.3,
  §10.1), never as a changed policy deadline; the bounded work inside the
  lock keeps it short, but no fixed bound is claimed. **Force exit never takes this lock**: it sets an atomic
  stop flag that the writer reads before every write call.
- **Writer**: writes each line completely: a partial write continues with
  the rest, `EINTR` is retried; before each write call (the first, each
  continuation and each retry) it checks the stop flag and the final-drain
  deadline below and begins no call once either is set or passed. It holds
  no lock while writing.
- **Final output**: after phase D the final warnings and summary are
  rendered and queued as one batch; the writer writes them after every
  earlier line, never interleaved. A complete stream ends with the job
  summary line (§2.8 templates) and its line feed.
- **Sink behaviour**:
  - *During the job* there is no stderr deadline: a slow or blocked sink
    delays only stderr; result lines wait in memory within the ceiling and
    progress is dropped as above.
  - *Final drain*: the origin is the **final-batch enqueue instant** t_q,
    the end of the queue critical section that queues the final batch.
    The **stderr final-drain deadline** is the absolute instant t_q + 5 s
    (*fixed*). The work between the end of D and t_q, rendering the final
    lines and acquiring the queue lock to queue them, is not covered: it
    does no I/O, rendering is bounded by the line and allowance limits, and
    lock acquisition has no bound (Queue lock above), so no policy deadline
    runs from the end of D. Every write call, partial write and `EINTR`
    retry made after t_q, and a call already in progress at t_q, counts
    against the same instant; nothing restarts it. cpf
    waits until the writer has written the last line, or has stopped on an
    error, or the deadline passes, whichever is first. At the deadline cpf
    stops waiting: the stop flag is set, so no further write call begins
    (as §5.4 force exit), a call already blocked is abandoned with the
    writer thread, and cpf exits with its status unchanged (§5.2), after
    the §5.3 re-raise where it applies. **Bound**: t_q → exit ≤ 5 s
    [+ 100 ms] plus the re-raise, for any sink; end of D → t_q is
    observed in fixtures ([100 ms]) but carries no deadline.
  - *Loss at the deadline*: a sink that is slow but not blocked can lose
    the tail too. Lost: every result line from the first one not completely
    written by the deadline on, the summary included; that first line may
    be cut, without its line feed. Progress lines are not involved (none is
    queued after E). The loss is not recorded in the report, the stdout
    document or the event stream (all final before D) and does not change
    the status. A reader tells it from stderr alone: the stream does not
    end with the summary line and its line feed (a cut last line, or a
    last line that is not the summary); the per-test lines present are
    then a prefix, in plan order, of the per-test lines of the delivered
    report's tests.
  - *Write error* (`EPIPE` included; cpf does not die of `SIGPIPE`): the
    writer stops at the first error; later lines are discarded without
    counting; the status is unchanged (§5.2); cpf stops waiting as soon as
    the writer has stopped.
  - *Orderly cancellation*: no change; lines already offered are written,
    the summary names the cancellation, and the §5.3 re-raise follows the
    final drain.
  - *Force exit* (E §8.2), independent of the final-drain deadline and
    available at any point, including between two partial writes of one
    line: the stop flag is set, no further write call begins, queued lines
    are lost and the line in progress may be cut, unreported; status §5.3.
- Non-job commands keep their defined stdout results (`cpf current`, `cpf
  switch`, `cpf test add`), diagnostics on stderr.
- Raw `cpf run`: the child's streams are forwarded (E §8.1); on any outcome
  other than `exited_zero` one summary line follows on stderr after
  forwarding finished, naming the child's actual termination (D2). Two
  separate clocks apply, each with its own origin: the **console flush
  deadline** (2 s from the drain milestone, E §5.5) bounds the forwarding
  flush, unchanged; the **raw final-diagnostic wait** (2 s from the enqueue
  instant of the summary line, E §5.5) replaces the stderr final-drain
  deadline for that line, with the same absolute-deadline and stop-flag
  rules. Neither clock restarts or extends the other.

## 9. Conformance fixtures

Deterministic: concurrency cases use injection points with barriers; write
failures are injected into the stdout or stderr writer at a chosen byte
offset with a chosen error. Each fixture asserts the named members, stdout
bytes where relevant, and the process status.

**Platform scope.** A fixture that runs a job command end to end with real
executions (`cpf test`, `cpf build`, `cpf cmd`, `cpf run`) runs on Linux and
macOS only. On Windows the same command lines assert the platform refusal
instead (class `infrastructure`, status 10, before admission, no job
directory; runtime decision record §3). Queue, credit, deadline and error
behaviour is also tested portably, on all three OSes, through the job engine
with an **injected executor** (fake runs with scripted outcomes) and, where
named, a **controlled writer** (each artefact write takes a scripted service
time or blocks on a gate) and a **controlled clock** (persistence deadlines,
credit waits and the budget B read an injected monotonic clock that only
the fixture advances). Controlled-clock results are exact; §10.1 allowances
apply only to real-clock observations.

Injection points: `cancel.intent`, `check.admit` (before admission),
`verdict.freeze` (before V), `fact.cutoff` (before the cutoff read),
`fact.after_cutoff` (between cutoff and replace), `fact.after_replace`,
`hook.before_start`, `settle.wait` (inside W), `finalize.snapshot` (before
F), `persist.report`, `persist.write` (each artefact write on the
persistence thread: the controlled writer's scripted service time or gate),
`persist.credit` (inside a credit wait), `persist.clock` (the controlled
clock), `events.enqueue`, `delivery.write`, `stderr.write`,
`stderr.lock` (holds the stderr queue lock across a barrier),
`manifest.publish`, `run_dir.create`.

### 9.1 Derivations (pure, table-driven)

| Fixture | Expected result | Test |
|---|---|---|
| `cpf build` on a build-free Python target (§2.3/§2.7) | `succeeded`, `build: null`, zero executions, exit 0 | `tests/cli_build.rs::execution::python_build_free_succeeds_without_running_the_script` |

- Every row of §§2.1–2.5 and §2.7, and an orphan example per invariant
  (`skipped{admission_closed}` without a `retention_budget` refusal,
  `stop_policy` in a test job, `exited_zero` + `failed{incomplete_output}`,
  build `skipped{stop_policy}`) → subresult `internal_error`,
  `result.internal.locus` set, job `internal_error`, 70, report finalized.
- §2.2: stdout `WriteError` with `Exited{0}` → `infrastructure`, cause
  `capture_loss{stdout, write_error}`; `Kill` breach plus independent
  `WriteError` → `infrastructure`, the breach visible in `violations` and
  `stdout.exceeded_at` (E §10.3); stderr `Discard` breach on a test run →
  `exited_zero`, note `truncated_stderr`; `Kill` breach on both streams →
  `output_budget`, `streams ["stdout","stderr"]`; nine distinct causes → 8
  listed in §2.2 order, `causes_dropped: 1`.
- §2.4/§2.5: `not_started{cancelled_before_start}` → check
  `not_admitted{run_not_eligible}`, verdict `cancelled`; answer absent +
  `exited_zero` → `unchecked`; answer absent + intent at `check.admit` →
  `cancelled`; answer absent + `skipped{build_not_succeeded}` → `not_run`;
  TLE + matching output → `wall_deadline`, `check.passed`, `diagnostic`;
  TLE + injected capture read error → `wall_deadline`, `check_error`,
  `diagnostic`; `exited_zero` + injected read error → `infrastructure`;
  invalid float answer → `checker_failed`.
- Signal under an installed address-space limit → `signal`, label `RE`,
  qualifier names the limit.
- Coverage: cancellation after every test was admitted and checked →
  `partial: false`, completed verdict, `cancellation.phase: "primary"`,
  fact `not_attempted{cancelled}`, hooks skipped, cancellation status; one
  `retention_budget` refusal → that test `infrastructure`, later ones
  `skipped{admission_closed}`, `partial: true`; counts satisfy §2.1.
- `run_dir.create` finding an existing directory → that execution
  `not_started{setup}`, `infrastructure`, its run id not reused, nothing
  written into the directory.

### 9.2 Finalization and cancellation (barriers)

- Intent at `fact.cutoff` → `not_attempted{cancelled}`, previous fact
  byte-identical, phase `fact`.
- Intent at `fact.after_cutoff` → the replace proceeds: `published`, phase
  `fact`, warning `late_cancellation`.
- Intent at `fact.after_replace` → `published`, phase `fact`.
- Intent at `hook.before_start` → that and later hooks `skipped{cancelled}`,
  phase `hooks`.
- A record persistence blocked at `settle.wait` for 6 s → its `Ref.error`
  is `timeout`, warning `persist`, report finalized after W with no pending
  reference.
- **Persistence credits, controlled writer and clock** (E §6.2; portable,
  injected executor, 3 000 executions, 5 concurrent producers enqueueing as
  early as the plan allows, so N = 3 000 and B = 46 s):
  - the controlled writer scripts the whole per-artefact cycle (E §6.2
    service assumption); work outside the scripted time takes no
    controlled-clock time;
  - service time 4 ms for every cycle → no `QueueFull`, no `Timeout`;
    every spec, record and `cleanup.log` published and `report.json` the
    last write; peak queued credits ≤ Q; every credit wait < 1 s; total
    credit wait ≤ B; every accepted artefact published by its enqueue call
    + 5 s;
  - 4 ms writes with one write of 1.2 s while the queue is full → the
    waits that reach 1 s end `QueueFull` at exactly call + 1 s, the queue is
    nonblocking from then on (a later short enqueue fails at once), those
    artefacts' refs are `queue_full`, every accepted artefact is published,
    no `Timeout`, report published; the same with every write 4 ms but a
    scripted 1.2 s pause after one credit release before the next write;
  - service time 6 ms for every cycle → no single wait reaches 1 s, the
    union of waits reaches B and the waits then in progress end `QueueFull`
    at that clock instant; nonblocking afterwards; no `Timeout`;
  - one write gated past its enqueue call + 5 s → `Timeout` at exactly
    that instant, every other pending artefact `timeout`, every current
    credit wait and later enqueue `worker_unavailable`.
- **Persistence throughput, platform evidence** (Linux and macOS, real
  filesystem, not a policy guarantee): `cpf test` on a build-free target
  with 3 000 trivial passing tests, unpadded writes. Asserts: no
  `QueueFull`; exactly 3 000 `spec.json`, 3 000 `record.json` and 3 000
  `cleanup.log` files; each spec decodes and its digest equals its
  `spec.digest`; each record decodes and names its spec's `Ref`; each
  `cleanup.log` starts `record finalized`; every `spec` and `record` `Ref`
  has a `path`; no `persist` warning; `report.json` is the last write,
  published and decodes to the delivered document without `persistence`;
  start of W → `job_finished` within the §2.9 combined bound. Q, the 1 s
  limit, B and every 5 s deadline are unscaled; only the observation
  allowances scale (E §10.1). The run records the maximum and 99th
  percentile of the per-artefact service cycle (credit release to credit
  release while work is pending) and the total credit wait, as data. A
  pass shows only that this run published everything; it does not show
  that the host meets the E §6.2 service assumption. A failure blocks CI
  until classified as a defect or as a recorded platform finding.
- **Saturation, then report** (portable, controlled writer): the first
  write gated until an enqueue has waited at `persist.credit` for 500 ms on
  the controlled clock, then released at 4 ms per write → no
  `PersistError`, every artefact published, `report.json` published and
  readable.
- **Stalled thread** (portable, controlled writer and clock; and on Linux
  and macOS with real executions): 100 trivial tests, `persist.write` gated
  forever → every `spec` and `record` `Ref` error is `timeout`,
  `queue_full` or `worker_unavailable`, with ≥ 1 `timeout`; every accepted
  artefact ends `timeout`; `persistence.report` is
  `failed{worker_unavailable}` and `job_finished.report` `null`; total
  credit wait ≤ 1 s (exact on the controlled clock, [+ 100 ms] on the real
  one); start of W → `job_finished` ≤ 5 s ([+ 300 ms] on the real clock);
  status by verdict (§5.2); the thread keeps its reservations (E §6.2).
- **Normal and late completion** (portable, controlled writer): a write
  returning before its deadline → its measured bytes committed once, by
  the persistence thread at the return, and its credits back in the pool;
  a write gated past its deadline, then released after the job handed the
  thread to the cleanup service → its credits returned to the closed pool
  at the return (not reissued), its measured bytes committed exactly once,
  by the cleanup service after the join, the queued artefacts' reservations
  released then; retained bytes equal the files on disk in both cases.
- **Empty accepted set**: a build-free `cpf build` (no execution, no
  artefact before the report) → W ends at once, `report.json` published,
  start of W → `job_finished` ≤ 5 s [+ 300 ms].
- **Cancellation in a credit wait** (portable, controlled writer): writes
  gated, an enqueue waiting at `persist.credit`, intent → that artefact's
  `Ref` (or, for `cleanup.log`, its `persist` warning) has error
  `cancelled`; gate released → artefacts accepted earlier are published,
  `report.json` published; encoder, decoder and the Python oracle agree on
  `{"kind":"cancelled"}`; cancellation status.
- Intent at `settle.wait` in a `test` job after hooks, and in a `build` job
  → recorded with phase `settlement`, verdict unchanged, settlement
  completes, warning `late_cancellation`, cancellation status.
- Intent at `finalize.snapshot` → recorded with phase `closure`; intent
  after F → not recorded, status by verdict.
- Intent while run preparation is paused before its first mkdir, and before
  deferred program completion → that execution is
  `not_started{cancelled_before_start}` with `run` set, `program` and `spec`
  `null` (§4.5), the job returns without waiting for the paused step, and
  the lease is released once it returns. Intent after preparation
  completed → `program` and `spec` non-null.
- Real signals: Ctrl-C during the build → `cancelled`, Unix death by
  `SIGINT`, Windows 6; `SIGTERM` → death by `SIGTERM`; API → 6; second
  Ctrl-C → force exit, death by `SIGINT` / 6.
- Injected panic in a test-derivation thread (before V) → `primary_verdict
  = verdict = internal_error`, fact `not_attempted{not_completed}`, no
  hooks, 70, report persisted and delivered; in a hook (after V, after a
  published fact) → fact stays `published`, `verdict: internal_error`,
  `primary_verdict` unchanged, later hooks `skipped{job_not_eligible}`, 70;
  in planning before readiness with `--json` → refusal document class
  `internal`, 70; in the report writer during S, before any stdout byte →
  stdout empty, 70; during delivery after the first stdout byte → no
  further bytes, 70.
- Injected supervisor panic after creation in a test run, so no record is
  produced → coverage `admitted`, run id and `spec` set, `record` and
  `summary` `null`, `invariant: record_missing`, test `class: null`, check
  `internal_error{record_missing}`, verdict `internal_error`,
  `Cov.admitted = 1`, `Cov.invalid = 1`, `by_class.unknown = 1`,
  `partial: true`, 70.
- Injected panic in a comparison (record present, `exited_zero`) → check
  `internal_error`, test verdict `internal_error`, `class` as observed, 70.
- Injected panic in a **diagnostic** comparison after a `wall_deadline`
  run → check `internal_error`, `diagnostic: true`, test verdict
  `internal_error` (not `wall_deadline`), 70.
- Injected panic while deriving the class of a present record → coverage
  `started`, `record` path set, `summary` `null`, `invariant:
  summary_underivable`, `class: null`, `by_class.unknown = 1`,
  `Cov.invalid = 1`, test verdict `internal_error`, 70.
- Injected panic while building the summary after the class
  `nonzero_exit` was derived → `summary` `null`, `invariant:
  summary_underivable`, `class` `nonzero_exit`, `by_class.nonzero_exit = 1`,
  `Cov.invalid = 1`, test verdict `internal_error`, 70.
- `cpf cmd` (no tests): injected panic while building the summary of one
  iteration after its class `exited_zero` was derived → that
  `executions[]` entry has `class: {"kind":"exited_zero"}`, `summary`
  `null`, `invariant: summary_underivable`; `by_class.exited_zero` counts
  it, `Cov.invalid = 1`, job verdict `internal_error`, 70.
- Injected panic while summarising a `not_started{retention_budget}`
  record → coverage `not_started`, `record` path set, `summary` `null`,
  `invariant: summary_underivable`, `class: null`, `by_class.unknown = 1`,
  `Cov.invalid = 1`, job verdict `internal_error`, 70.
- `after_test` hook: ordinary failure → warning, status by verdict;
  injected panic in its supervisor → hook outcome `internal_error`,
  `verdict: internal_error`, `primary_verdict` unchanged, 70.
- Orphan `cmd` execution (`skipped{admission_closed}` without a
  `retention_budget` refusal) → coverage serialized as observed,
  `invariant` set, `Cov.invalid = 1`, `result.internal.locus =
  execution{index}`, job `internal_error`, 70.
- `cpf switch` with `on_switch` hooks: a failing hook → switch 0, `hook` job
  verdict `nonzero_exit` in its `report.json`, stderr warning; hook job
  admission refused (`job-seq` malformed) → switch 0, warning, no hook run;
  Ctrl-C during a hook → cancellation status; injected panic in the hook
  job → 70; the saved selection is the new one in every case.

### 9.3 Transport

- `cpf test --json`, passing: stdout is one line starting
  `{"schema":1,"document":"job_report","platform":`, ending `}\n`, equal to
  `report.json` plus `persistence`.
- `persist.report` failing → stdout document complete,
  `persistence.report.kind = "failed"`, `job_finished.report` `null`,
  status by verdict.
- `delivery.write` failing with `EPIPE` at offset 0 and at offset 100 → 11,
  `report.json` present; with `ENOSPC` → 11; with a signal-origin
  cancellation snapshot → 11, no re-raise. stdout closed before start → 11,
  no job directory.
- `cpf run --json`, `cpf run --events`, `cpf test --json --events` → 2.
- `--events`: header first; `seq` from 0, strictly increasing; with
  `events.enqueue` dropping every third event over 10 000 runs, the gap
  equation holds and `job_finished`, `report` are the last two lines; no
  `output_chunk` without `=output`; with `=output`, a chunk boundary inside
  `é` decodes to the original bytes; `delivery.write` failing mid-stream →
  job completes, 11.
- Stderr fixtures with real executions run on Linux and macOS (§9 platform
  scope; Windows asserts the refusal for the same command lines); the queue
  fixtures marked portable drive the stderr queue from the job engine with
  an injected executor and a sink double on all three OSes.
- `stderr.write` blocked at a barrier during a `cpf test` job of 3 000
  trivial tests (each per-test line ≥ 40 bytes, so > 64 KiB is offered) →
  the job finishes and delivers its document while blocked; released 1 s
  after t_q → exactly 3 000 per-test lines in plan order, then the final
  warnings, the summary last; `progress_dropped.lines` counts only
  progress lines.
- **Drop finality across E, S, D** (portable): sink double blocked from
  before E until after D with progress queued, then released 1 s after
  t_q →
  `progress_dropped.lines` in `report.json`, the stdout document and the
  final `report` event of `--events` are equal, and equal progress
  offered before E minus progress the writer took before E; no progress
  line is written after release; the per-test lines and summary follow.
- **Full result ceiling** (portable): 10 000 planned tests (repeat 1, the
  C §8.2 maximum), each per-test line exactly 512 B, the final allowance
  filled (64 diagnostics, 29 warning lines, 16 summary lines; stress
  aggregates in A4), sink double blocked until after D → peak queued
  result bytes ≤ 5 251 072 B and the debug assertion holds; released → all
  10 000 per-test lines and the final batch written once, in sequence
  order.
- Build-free `cpf build` → its summary is written through the final
  allowance.
- **10 000-execution progress flood**, two forms:
  - Linux and macOS, real executions: a selected problem with ten enabled
    tests and `[commands.flood]` with `argv` naming the trivial fixture
    program (exits 0, no output), `for_each_test = true`, `repeat = 1000`
    (C §8.1, §8.2: 10 × 1 000 = 10 000 planned runs, the maximum; no
    `needs_build`, so no build). `cpf cmd flood` with `stderr.write`
    blocked from the first run, released 1 s after t_q → status 0; stderr
    holds, in order, any start-up configuration diagnostics, exactly ten
    per-test lines (one per test, label `OK`, §2.8, with the per-test
    aggregates of C §8.2) in plan order, then the final warnings including
    `progress_dropped` with `lines` > 0, the summary last; only progress
    lines are missing, and `progress_dropped.lines` equals the progress
    lines offered minus those written. Windows: the same command asserts
    the refusal (status 10).
  - Portable, injected engine: the same plan (ten tests × 1 000 iterations)
    from an injected executor with a sink double → the same assertions.
  (Stress: A4 adds the stalled-then-resumed stress case with its final
  aggregates.)
- `stderr.lock` held while another thread offers with free space
  (portable) → the line is queued, not dropped, once the lock is released;
  exactly the offered lines are written. A force exit while `stderr.lock`
  is held completes without the lock (§5.3 status).
- Sink double writing 1 byte per call (partial writes), and one returning
  `EINTR` before every successful call (portable) → output byte-identical
  to an unrestricted sink.
- **Final-drain deadline** (portable sink double; real clock; every bound
  measured from t_q, recorded by a debug milestone at the end of the
  enqueue critical section, never from the end of D): sink blocked
  forever after the job → exit 5 s [+ 100 ms] after t_q with the status by
  verdict, no summary written; sink taking 2 s per write call → exit 5 s
  [+ 100 ms] after t_q, exactly the first two lines of the final batch
  written (the third call, begun about t_q + 4 s, is abandoned), the rest
  lost, the stream not ending with the summary line; sink writing 1 byte
  per 10 ms call (partial writes) and one returning `EINTR` every 10 ms →
  exit 5 s [+ 100 ms] after t_q (retries and continuations do not extend
  it); `stderr.lock` held for 1 s between the end of D and t_q → exit still
  5 s [+ 100 ms] after t_q (the deadline starts at t_q, not at D); a second
  Ctrl-C 1 s into the final drain, and one between two partial writes of a
  line → force exit at once, §5.3, no further write call.
- Raw `cpf run` (Linux and macOS): child exiting non-zero with the stderr
  sink blocked after the drain milestone → forwarding flush ends by the
  console flush deadline (drain milestone + 2 s [+ 100 ms]); the summary
  line is then queued and exit follows its enqueue instant + 2 s
  [+ 100 ms] (raw final-diagnostic wait), each measured from its own
  origin.
- `stderr.write` failing with `EPIPE` at the first and at the 100th line →
  no later stderr bytes, status by verdict; Ctrl-C during the job with a
  blocked sink, then released → the per-test lines offered before intent,
  then a summary naming the cancellation, then the §5.3 status.
- Refusals: phase-1 config error → `refused`, class `config`, 7,
  `persistence.report.kind = "not_attempted"`; `SourceCollision` after
  registration → `refused`, `job` set, class `workspace`, 8, no
  `report.json`; `manifest.publish` failing → `refused`, class
  `workspace`, 8.
- Non-job: `cpf config show --json` on invalid configuration → `error`
  document, class `config`, 7.

### 9.4 Encoding and bounds

- `ns`: Unix `61 2F C3 A9` → `{"utf8":"a/é"}`; `61 0A` →
  `{"bytes_b64":"YQo="}`; `61 FF` → `{"bytes_b64":"Yf8="}`; Windows `D800`
  → WTF-8 `ED A0 80` → `{"bytes_b64":"7aCA"}`; empty → `{"utf8":""}`.
- `nsum(4)` of `abcdefgh` → head `{"utf8":"abcd"}`, `len "8"`, `truncated
  true`, digest of all 8 bytes; of `abé` → not truncated; of `abcé` → head
  `abc`, truncated.
- Readers reject: duplicate member, `"-0"`, `"01"`, `1.0` in a `u32` field,
  a number in a `u64` field, `"Yf9="`, `{"bytes_b64":"YQ=="}`.
- `u64::MAX` → `"18446744073709551615"`; exit code `0xC0000005` →
  `3221225477`, `ntstatus` `status_access_violation`.
- Worst cases serialised for every row of §3.7 (all caps full, every string
  at its maximum, `"`-heavy UTF-8 values, base64 values) stay within the
  stated bounds; 10 000 planned executions within 64 KiB + 12 KiB × N.
- A `report.json` with `schema` 2 is never pruned, identified from its
  prefix.

### 9.5 Display

- The label of every verdict and build result is exactly its §2.8 entry;
  each job summary equals its template; `passed` with cleanup `Unverified`
  → "OK (cleanup not verified)".

## 10. Amendments to other documents

Amendments applied to the other documents with this contract.

- **E §6.2 (Persistence)**, add: "A job settles its persistence before
  finalizing its report: it waits, outside the verdict lock, until every
  artefact it enqueued (specs, records, first `cleanup.log` publications)
  has a result, published or `PersistError`; this ends by the last enqueue
  + 5 s. The report then names a path for every published artefact and an
  error for every other (report §2.9 W)." Replace the size bound "report ≤
  16 KiB + 64 KiB × runs" by "report ≤ 64 KiB + 12 KiB × planned
  executions (report §3.7)".
- **E §5.4**, add after the metadata reservation: "Before its first run the
  job also reserves the report bound of report §3.7 for `report.json` and
  its temporary file."
- **E §6.4**, replace "A comparison already started finishes …; one not
  started when intent is set is skipped" and "The job report says
  `cancelled` and is still finalized" by: "Intent arbitrates check
  admission per report §2.4. A check I/O failure is infrastructure unless
  the check is diagnostic, in which case it is evidence only. The job
  verdict follows the precedence of report §2.7 (`internal_error`, then
  `infrastructure`, then `cancelled`, then the outcome rows): it is
  `cancelled` only if no higher-precedence value applies and some primary
  execution or check was stopped, skipped or refused by cancellation;
  intent that stopped nothing leaves the completed verdict. Every intent
  set before report finalization is recorded with its phase (report §2.9)
  and the process status follows report §5.2. The report is still
  finalized."
- **E §8.2**, replace "exit with the cancellation exit code" by "terminate
  per report §5.3 (Unix: re-raise the forcing signal; otherwise 6)", and add
  "no further intentional write to stdout or stderr begins after force exit
  starts".
- **E §9**, replace the `RunFinished` bound "≤ 4 KiB" by "≤ 7 KiB (report
  §3.7)"; add: "`seq` starts at 0 and is assigned in one critical section
  with the enqueue-or-drop decision; `JobFinished` takes the next number
  after the main channel is closed. The terminal slot is filled after
  report persistence (report §2.9 S), so `JobFinished.report` reflects it.
  `RunStarted` carries `execution` and `role` on the wire."; replace
  "Otherwise `None`, and the report's `PersistError` says why." by
  "Otherwise `None`; the reason is in the delivered document's
  `persistence` member (report §4.11) and on stderr, never in
  `report.json`, which cannot contain its own persistence result."; replace
  "Transport failure (closed stdout, `EPIPE`) gives the report-delivery
  exit code (report transport)." by "Transport failure (closed stdout,
  `EPIPE`) is a delivery failure; the process status follows report
  §5.2."
- **E §6.4 / §5.3** (budgets), add: "A `Discard` breach on a stream that is
  not compared is a note, not an output-budget outcome (report §2.2)."
- **E (spec record)**: `spec.json` encodes native values as report §3.3
  `ns`. `spec_digest` is unaffected (it is computed over E §2's binary
  encoding). If `spec.json` has already been written with another native
  encoding, this is a `spec.json` schema increment in E.
- **S §6.1**: in the layout, `<run-id>` is `r` + five-digit admission index
  (report §3.4), `builds/<build-id>` uses the build's run id, and
  `inputs/capture.json` is added: "capture manifest (report §4.2)".
- **S §6.2**, replace "The capture check and the captured file lists live
  in the ready job value and reach reports (report schema) and facts (§6.5);
  they are not persisted here." by "The capture check and the captured file
  lists are persisted in the capture manifest `inputs/capture.json`
  (report §4.2), published no-clobber as the last capture step with its
  size reserved first; a manifest that is not committed is a capture
  failure and the job never becomes ready. They are not persisted in
  `job.toml`. Reports carry bounded previews, the list digest and the
  manifest path; facts carry the capture check (§6.5)."
- **S §6.4**, add to the per-job accounting list: "the capture manifest and
  the report reservation (report §3.7)".
- **S §6.5**, replace "Publication at job finalization of a job that
  completed (not cancelled, not failed before its report)" by "Publication
  in report phase A by a job whose primary verdict is not `cancelled`,
  `infrastructure` or `internal_error`"; insert before the decision table:
  "After reading `job-seq` and the existing fact, the job reads its
  cancellation intent once (the **cancellation cutoff**); intent set →
  `NotAttempted{Cancelled}`, nothing written. Intent set after the cutoff
  does not prevent the replace."; replace "A job cancelled or interrupted
  before the commit leaves the previous fact unchanged" by "A job cancelled
  before the cutoff, or interrupted (death) before the commit, leaves the
  previous fact unchanged; cancellation after the cutoff does not stop the
  replace, and a committed fact is never rolled back."
- **S §5.2** step 5 and **S §5.3** step 4: "exit 1" → failure class
  `partial` (9). **S §5.7**: "exit code is 1 (`NoSelection` included) until
  the report transport assigns codes" → "failure class `workspace` (8)".
- **C §8.1**, replace the exit-status paragraph by: "Exit status of `cpf
  cmd`: selected by report §5.2, whose precedence governs. In its terms:
  usage 2; forced cancellation per report §5.3; an internal failure 70; a
  delivery failure of `--json`/`--events` 11; any recorded cancellation
  (including one after every run completed) per report §5.3; plan errors 7;
  workspace errors 8; job verdict `infrastructure` 10; a failed build
  (`needs_build`) 3; a run failure (§8.2) 1; otherwise, when every planned
  run succeeded, 0."
- **C §8.2**, replace "remaining runs recorded `skipped`" by "remaining
  runs have coverage `skipped{stop_policy}` (report §2.1)"; add: "Planned
  runs count only hooks allowed by trust, the switch and the depth guard at
  plan time; a cancellation records remaining runs as
  `skipped{cancelled}`."
- **C §8.3**, replace "`after_test` once per `cpf test` invocation after its
  job finished, also when tests failed, never after cancellation or an
  infrastructure failure of the job" by "`after_test` in report phase H,
  after fact publication, when the primary verdict is not `cancelled`,
  `infrastructure` or `internal_error` and no cancellation was requested";
  add: "An `after_test` hook's ordinary failure (a failed, cancelled or
  skipped hook) is a warning in the triggering report; a hook internal
  error is an internal failure of the job (report §2.6, status 70).
  `on_switch` hooks run as a job of kind `hook` (report §2.10). Its
  ordinary failures (verdicts `wall_deadline`, `output_budget`, `signal`,
  `nonzero_exit`, `infrastructure`) and its admission failures of class
  `workspace` or `config` are warnings and leave the status of `cpf switch`
  at 0; a cancellation selects the cancellation status and an internal
  failure (verdict `internal_error` or a caught panic) selects 70, by the
  precedence of report §5.2."
- **C §10**, replace the JSON bullet by: "JSON follows report §3 (canonical
  profile, reader rules). Mapping of configuration values: TOML string →
  JSON string; integer → decimal string (`i64`); float → `dec` (`{"text",
  "bits"}`), for every float, not only comparator fields; boolean → JSON
  boolean; array → JSON array; table → JSON object in key order of the
  effective configuration; native paths and values (program paths,
  environment values, file paths) → `ns` (`{"utf8"}` iff valid UTF-8
  without control characters, else `{"bytes_b64"}`; Windows WTF-8). TOML
  date-times are not accepted by any configuration key (phase-1 error), so
  they need no mapping. The document has `schema`, `document:
  "config_show"` and `platform`." Replace "Exit 0 or the config-error code
  (report transport)" by "exit 0 or 7".
- **`src/exit.rs`, `src/error.rs`**: replace the provisional `Failure = 1`
  with §5.1 and failure classes; `Cargo.toml` release profile keeps
  `panic = "unwind"`.
- **Checkers design** (CLI exit codes "belong to the report transport"): no
  text change; §5 provides them.

## 11. Decisions

Decided (user, 2026-11-21):

- **D1** Exit codes 0–11 plus 70 (§5.1).
- **D2** Raw `cpf run` uses fixed statuses; the child's status is reported,
  never passed through.
- **D3** Unix signal-origin cancellation re-raises `SIGINT`/`SIGTERM` after
  orderly cleanup and delivery; API cancellation and Windows exit 6.
- **D4** `--events` on stdout, exclusive with `--json`, report as final
  envelope.
- **D5** Job commands' human output on stderr in every mode.
- **D6** Compact JSON, 64-bit integers as decimal strings, run summaries
  with record references.
- **D7** `cpf test` with zero selected tests builds, then `no_tests`, 4.
- **D8** A `Discard` breach on a stream that is not consumed is a note.
- **D9** Delivery failure (11) outranks orderly cancellation; only forced
  cancellation keeps precedence over it.
- **D10** Output chunks only with `--events=output`.
- **O1** Run ids `r` + five-digit admission index, once per admitted
  execution including builds and hooks, never reused, existing directory →
  `not_started{setup}` (§3.4).
- **O2** Full capture manifest persisted as `inputs/capture.json` with
  reservation and publication semantics; bounded previews, digest and
  reference in the report (§4.2).
- **O3** One ordered stderr stream through one writer: sequence-numbered
  result lines that the queue never drops, within a bounded ceiling; a
  64 KiB lossy progress budget whose queued remainder is detached and
  counted at E, where the drop counter is final; lines ≤ 512 bytes; a plain
  queue lock with bounded work inside it, never taken by force exit; and an
  absolute 5 s final-drain deadline from the final-batch enqueue instant,
  past which the unwritten tail is lost, visible only as a stream not
  ending with the summary (§8).

No open decisions remain.
