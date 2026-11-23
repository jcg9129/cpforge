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
  `PersistError`. Bounded: each artefact settles by its enqueue time + 5 s,
  and a timeout fails every queued artefact at once (E §6.2), so W ends by
  the last enqueue + 5 s. All `persist` and `durability` warnings are final
  at W. Later physical-cleanup facts go to `cleanup.log` only (E §5.2),
  never into the report.
- **E Producer closure**: the event channel is closed and `dropped`
  computed (E §9); the progress queue (§8) is closed and its drop counter
  finalized.
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
  `result.pre_delivery_exit` (§5.2) and freezes the report. Intent after F
  is not recorded; a later notification counts only towards force exit
  (E §8.2).
- **S Persistence** of `report.json` (E §6.2, ≤ 5 s), then the terminal slot
  receives `JobFinished` (report id iff the file was seen published).
- **D Delivery** (§6, §7), then the final stderr summary (§8), then the
  process status (§5).

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
| `warnings` | `[Warning; 24]` | §4.10, first 24 in occurrence order |
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
  `{"kind":"worker_unavailable"}`, `{"kind":"io","os_error":i32?,
  "message":text}`.
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
`durability` (`ok`, `uncertain`) is `null` unless `published`.
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
| 2 | `usage` | invalid arguments (clap) or a rejected combination: `--json`/`--events` with raw `cpf run`, `--json` with `--events`, `--problem` with `switch`, both test sources on stdin |
| 3 | `build_failed` | the build failed |
| 4 | `not_verified` | nothing failed but not everything was checked: `unchecked`, `no_tests` |
| 5 | `checker_failed` | judging could not decide (schema 1: invalid answer) |
| 6 | `cancelled` | cancellation (API, Windows, or Unix when not re-raised, §5.3) |
| 7 | `config` | failure class `config`: C §3 phases 1–3, unsupported problem kind (C §9), missing context (C §8.1), accident guard (S §7) |
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
| *(draft)* `cpf status` | 0, 8, 11 (`--json`) |
| *(draft)* `cpf clean` | 0, 8; `--apply`: 9 when a selected candidate could not be renamed or deleted (held leases are skipped) |
| *(draft)* `cpf fsck` | 0, 8 (findings are reported with 0) |
| *(draft)* `cpf adopt`, `ignore`, `gitignore` | 0, 8 |
| `cpf --help`, `--version` | 0 |

"Cancellation" is the §5.3 status. Raw `cpf run` never passes the child's
code through; the child's status is in the report and the stderr summary
(D2). `cpf check` (design §5.4) gets its row with its feature.

## 6. `--json`

### 6.1 Availability

- Accepted by `cpf build`, `cpf test`, `cpf cmd`, and by non-job commands
  whose contract defines a document (schema 1: `cpf config show`, *(draft)*
  `cpf status` below; later `cpf test ls`). A per-command flag; elsewhere
  clap rejects it (2).
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
- One stderr writer thread per invocation serializes all human output.
  Progress and per-test lines are encoded first, each cut to ≤ 512 bytes
  (ending with `…`), and offered to a 64 KiB non-blocking queue; a line that
  does not fit is dropped and counted. The queue is closed and the counter
  finalized in phase E (warning `progress_dropped`).
- The final summary goes through the same writer after phase D (after
  queued lines, never interleaved), blocking like any CLI; force exit stays
  available. Stderr write errors are ignored (§5.2).
- Non-job commands keep their defined stdout results (`cpf current`, `cpf
  switch`, `cpf test add`), diagnostics on stderr.
- Raw `cpf run`: the child's streams are forwarded (E §8.1); on any outcome
  other than `exited_zero` one summary line follows on stderr after
  forwarding finished, naming the child's actual termination (D2).

## 9. Conformance fixtures

Deterministic: concurrency cases use injection points with barriers; write
failures are injected into the stdout or stderr writer at a chosen byte
offset with a chosen error. Each fixture asserts the named members, stdout
bytes where relevant, and the process status.

Injection points: `cancel.intent`, `check.admit` (before admission),
`verdict.freeze` (before V), `fact.cutoff` (before the cutoff read),
`fact.after_cutoff` (between cutoff and replace), `fact.after_replace`,
`hook.before_start`, `settle.wait` (inside W), `finalize.snapshot` (before
F), `persist.report`, `events.enqueue`, `delivery.write`, `stderr.write`,
`manifest.publish`, `run_dir.create`.

### 9.1 Derivations (pure, table-driven)

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
- `stderr.write` blocked at a barrier during a `cpf test` job of 3 000
  trivial tests (each progress line ≥ 40 bytes, so > 64 KiB is offered) →
  the job finishes, `progress_dropped.lines` > 0, the final summary appears
  after the barrier is released and is not interleaved.
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
- **O3** 64 KiB non-blocking progress queue, lines ≤ 512 bytes, drop counter
  finalized at E, final summary through the same stderr writer (§8).

No open decisions remain.
