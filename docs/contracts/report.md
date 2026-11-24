# Report contract

Status: v1 (decisions of 2026-11-21, §11), schema 1 reconciled with
every gate-2 addition (Appendix A). Normative for the `report` module, the
result derivation in `jobs`, and the process exit status of every `cpf`
command. It is the **report transport** and **report schema** that the
other contracts refer to: execution contract §§6.2, 6.4, 8, 9; storage
contract §§5.2, 5.3, 5.7, 6.1, 6.2, 6.5; configuration contract §§8.1–8.3,
10; `../design-checkers.md` (CLI exit codes); design §6.1 and §8. References
labelled *E*, *S*, *C*, *compare spec*, *checkers design* and *workspace
design* point to `execution.md`, `storage.md`, `config.md`,
`../compare-spec.md`, `../design-checkers.md` and `../design-workspace.md`;
the *stress*, *submission*, *archive* and *sites* contracts are
`stress.md`, `submit.md`, `archive.md` and `sites.md`.

MUST / MUST NOT / never are requirements. This contract derives results
from records the other contracts define, encodes them, and maps them to
exit statuses. Where it needs a change in another contract, §10 gives the
amendment text. `../commands.md` documents every command and summarises
the exit codes of §5.

**Scope of schema 1.** Every value a schema-1 reader must know is in this
contract; Appendix A lists them all. Schema 1 covers:

- built-in batch comparison (compare spec): `batch` problems, comparators
  `exact`, `tokens`, `lines`, `float`;
- judged problems (configuration contract §9.1): checkers and validators
  (profiles `testlib`, `simple`), answer policies, scores, presentation
  errors and interactive sessions (checkers design §§1–6), with roles and
  stages (§1, §4.4, §4.5), the batch and interactive derivations
  (§§2.11–2.13), their verdicts and exit rows (§2.7, §5), display profiles
  (§2.8) and fixtures (§9.6); names follow checkers design §1;
- file-based I/O (C §6.2 `[config] io`): the collection outcome of the
  output file and its place in the batch derivations (§2.15, with §§2.1,
  2.2, 2.4, 2.5, 2.11), labels (§2.8), the members `target.io` and
  `tests[].output_file`, the check kind `output_rejected`, the warning
  `io_scratch` (§§4.3, 4.6, 4.10) and fixtures (§9.8); staging and
  collection are E §5.6;
- stress jobs (the stress contract defines what they report): the job
  kind `stress` (§1), its verdicts (§2.14), the types `irid` and `fid`
  (§3.2), the bound B_S (§3.7), the `stress` member (§4.14), the
  `stress_failures` document (§4.15), exit rows (§5), iteration events
  (§7.3), stress result lines (§8) and fixtures (§9.7);
- submission (the submission contract holds the semantics and the
  `Submission` type, its §9.2): the job kind `submit` of `cpf submit-prep`
  in stage mode, its verdicts `submit_version_mismatch` and
  `submit_failed` (§2.7), version publication in phase A (§2.9), the
  `submission` member and stage tokens (§4), warnings (§4.10), exit rows
  (§5), the source-copy `submit` document and the status member `submit`
  (§6.1);
- the non-job documents of §6.1: `config_show` (C §10), `status`, the
  organisation documents `ls`, `search` and `notes` (storage contract
  §5.9), the archive documents `archive_plan`, `archive` and `restore` and
  the snapshot results in `ls` and `search` (archive contract), and the
  `fetch` document (sites contract §11.2, storage contract §5.8), with the
  `error` document of every non-job command (§4.12);
- the failure class `site` and exit code 12 for `cpf fetch`, kept
  conflicts as code 9, and the precedence of commands that act on several
  problems or run several jobs (§§1, 5.1, 5.2).

Schema 2 is not needed for any of them. The archive's snapshot manifest
and its other records use the §3.1 writer profile and the §3.5 reader rules
with the archive contract's own size bounds (archive contract §4.4); they
are not `--json` documents.

## 1. Terms

- **Job kinds**: `build` (`cpf build`), `test` (`cpf test`), `run` (raw
  `cpf run`, E §8.1), `cmd` (`cpf cmd <name>`), `hook` (the `on_switch`
  hooks of a successful `cpf switch`, C §8.3, §2.10);
  `stress` (`cpf stress` in every mode, stress contract §2); `submit` (`cpf submit-prep` in stage mode, submission contract
  §6; every planned execution of it is primary). A stress
  job's planned executions are its builds (C §8.2 stress plans); its
  iteration runs (E §1) are not planned executions, have no plan index or
  run id, never appear in `executions[]`, and are reported through the
  `stress` member (§4.14) and the iteration events (§7.3).
- **Planned execution**: one entry of the plan (C §8.2 planned runs):
  builds, tests × repeats (or repeats), the raw run, hooks; in a judged plan also the role builds and every test's
  validator, checker and interactor runs (C §8.2). It has a **plan
  index** (from 0, plan order) and a **coverage** (§2.1) independent of its
  outcome. An execution that is admitted also gets a **run id** (§3.4).
- **Primary executions**: for job kinds `build`, `test`, `run`, `cmd`, every
  planned execution except hooks; for kind `hook`, its hook executions.
  Only primary executions aggregate into the job verdict. The `after_test`
  hooks of a `test` job never do.
- **Run class** (§2.2), **check outcome** (§2.4), **test verdict** (§2.5),
  **job verdict** (§2.7).
- **Judge roles**: checker, validator, interactor (E
  §2.1); a **judge run** is a run of one of them, a **role build** the
  build of a built role (C §9.1.2). A **judged test** is a test of a
  problem whose `[check]` declares a role (C §8.2); a **batch judged test**
  is one of a problem whose effective kind is not `interactive` (§2.11);
  an **interactive test** is decided by its session (§2.12). Judge runs and
  role builds are primary executions.
- **Report**: the job's in-memory document (E §9), immutable from phase F
  (§2.9); `report.json` is its persisted copy (S §6.1); the **stdout
  document** is the report plus `persistence` (§4.11).
- **Command failure class** of any command (job or not): `usage`,
  `config`, `workspace`, `partial`, `cancelled`, `internal`, `site` (`cpf fetch` only, §5.1), or none.
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
| `submit_not_succeeded` | job kind `submit`, for its submit stage (after a probe that failed or mismatched), artifact build, role builds and test executions (reduced by §2.3, §2.4, B2, B10 and I2 like `build_not_succeeded`, giving `skipped{submit_not_succeeded}` and `not_run{submit_not_succeeded}`): an earlier probe or submit stage of the job has a class other than `exited_zero`, the probe mismatched, or (for executions after the submit stage) the artifact failed the selection rule; a skipped submit stage is a primary execution like the others (§2.1 reconciliation) and never decides the verdict, the probe does (§2.7 rows 2–3a) (submission contract §§6.3–6.4) |
| `input_not_validated` | a solution, interactor or checker run of a test whose validator run has a final record that does not report the input valid (§2.11) |
| `not_eligible` | a checker run whose test's solution run is not eligible for checking (§2.11), or whose output file was not collected (§2.15), or a follow-up checker after `CheckerSkipped{NotEligible}` (§2.12) |

Reasons apply in this order when several conditions hold:
`cancelled`, `submit_not_succeeded`, `build_not_succeeded`, `admission_closed`,
`input_not_validated`, `not_eligible`. Inside a judged test the runs are
admitted in plan order (validator, solution, interactor with the solution,
checker), each only after the earlier one's record is final.

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

- **Consumed stream**: the stdout of a test run; for a
  problem with `io.output` (C §6.2) stdout is not consumed (its output is
  the output file, whose over-budget outcome raises the test class,
  §2.15), while a `Kill` breach of it is still row 4 and its row-1 losses
  are still infrastructure. A `Discard` breach on a
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
| `skipped{build_not_succeeded}` (a role build after an earlier build that did not succeed) | – | `skipped{build_not_succeeded}` |
| `skipped{admission_closed}` (a role build after a `retention_budget` refusal) | – | `skipped{admission_closed}` |
| `skipped{submit_not_succeeded}` (the artifact build or a role build of a `submit` job after a probe or submit stage that did not reach a selected artifact) | – | `skipped{submit_not_succeeded}` |

Any other build coverage is an orphan (`internal_error{invariant}`). A
build that is not `succeeded` starts nothing that depends on it (design
§6.6); its dependants are `skipped{build_not_succeeded}`.

**Role builds** use the same table, each with its own
result (§4.4 `roles[].build`). In a judged job each build depends on the
builds before it in plan order (solution, checker, validator, interactor)
and every test execution depends on every build: a build that is not
`succeeded` makes the later builds and all test executions
`skipped{build_not_succeeded}` (in a `submit` job,
when the probe or the submit stage did not reach a selected artifact,
every build and test execution is `skipped{submit_not_succeeded}`
instead), except that an execution whose admission
comes after cancellation intent is `skipped{cancelled}` (build result
`cancelled`), which takes priority by the §2.1 order. A skipped build result never decides a
job verdict; the earlier build that did not succeed does (§2.7 rows 2–4
take every build result, solution and role). With zero selected tests the
builds still run and a failed role build gives `build_failed`, as for the
solution build (decision D7). `cpf build` builds the solution target
only.

`cpf build` on a build-free target (e.g. Python without a build stage)
performs no executions and yields job verdict `succeeded` (§2.7),
`build: null` (§4.4), and exit status 0 (§5.2).

### 2.4 Check outcome (test jobs)

This section and §2.5 derive tests of kind `compare`
without a validator. A batch judged test is derived by §2.11 (whose rows
end in this section for kind `compare` with a validator), an interactive
test by §2.12.

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
   3a. **Output file** (problems with `io.output`):
   a collection other than `collected` gives the collection check of
   §2.15 (`output_rejected` or `check_error`) and nothing else runs, before
   and whatever the answer state.
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
| `skipped{build_not_succeeded \| admission_closed \| submit_not_succeeded}` | – | `not_admitted{run_not_eligible}` | `not_run{reason}` |
| any | any or none | `internal_error{invariant}` | `internal_error` |
| `admitted` | none | `internal_error{record_missing}` | `internal_error` |
| `not_started{≠ cancelled_before_start}` | `infrastructure` | `not_admitted{run_not_eligible}` | `infrastructure` |
| `not_started{cancelled_before_start}` | `cancelled` | `not_admitted{run_not_eligible}` | `cancelled` |
| `started` | `infrastructure` | `not_admitted{run_not_eligible}` | `infrastructure` |
| `started` | `cancelled` | `not_admitted{run_not_eligible}` | `cancelled` |
| `started` | any other | `not_admitted{cancelled}` | `cancelled` (E §6.4: comparison skipped by cancellation) |
| `started` | `wall_deadline`, `output_budget`, `signal`, `nonzero_exit` | `unchecked`, `passed`, `failed`, `checker_failed`, `check_error`, `output_rejected` (all diagnostic) | the class |
| `started` | `exited_zero` | `check_error` | `infrastructure` |
| `started` | `exited_zero` | `checker_failed` | `checker_failed` |
| `started` | `exited_zero` | `output_rejected` | `wrong_answer` |
| `started` | `exited_zero` | `failed{reason ≠ incomplete_output}` | `wrong_answer` |
| `started` | `exited_zero` | `unchecked` | `unchecked` |
| `started` | `exited_zero` | `passed` | `passed` |

Further test verdicts, produced only by §§2.11–2.12:
`presentation_error`, `scored`, `session_limit`, `idle`.

For a problem with `io.output` the Class column is the
test class of §2.15 (equal to the run class unless the output file is too
large).

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
2. `infrastructure`: the build result (or any role
   build result), any test verdict, or any `cmd`, `run` or `hook`-job class
   is `infrastructure`.
3. `cancelled`: the build result (or any role build result), any test
   verdict, any such class, or any
   primary coverage is cancelled (`skipped{cancelled}`,
   `not_started{cancelled_before_start}`).
   3a. `submit_version_mismatch`: a `submit` job's
   version probe mismatched (submission contract §3.3). 3b.
   `submit_failed`: a `submit` job's probe or submit stage has class
   `wall_deadline`, `output_budget`, `signal` or `nonzero_exit`, or its
   artifact failed the selection rule (submission contract §6.4). Rows 2
   and 3 already count the probe's and the submit stage's classes and
   coverage (they are primary executions of the `submit` job, read as the
   "classes of other kinds" there). Rows 4–16 then apply
   to a `submit` job as to a `test` job (its artifact build, role builds
   and test verdicts).
4. `build_failed`: the build result, or any role build
   result, is `failed`.
5. `wall_deadline`; 6. `session_limit`, 7. `idle`;
   8. `output_budget`, 9. `signal`, 10. `nonzero_exit`: the first present
   among test verdicts (`test`) or classes (other kinds), in this order.
11. `checker_failed`, 12. `wrong_answer`, 13.
    `presentation_error`, 14. `unchecked`: among test verdicts.
15. `no_tests`: a `test` job with zero selected tests (decision D7: still
    built; a build failure gives `build_failed`).
16. `scored`: a `test` job on a problem with a points
    `scoring` (C §9.1.5) whose every test verdict is `passed` or `scored`;
    `result.score` (§2.13) says whether the aggregate is full.
17. `passed` (`test`, `submit` with the artifact
    test) or `succeeded` (`build`, `run`, `cmd`, `hook`, `submit` without it).

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
  `passed`, on a problem without a points `scoring`. Persistence, durability and cleanup warnings, fact outcomes
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
| `presentation_error` | `PE` (display `cpf`), `WA` (display `judge-cf`) | "presentation error", then the judge's role and message head |
| `scored` | `SCORE` | "<raw>/<scale>" (per test); job: "<value>/<max> (<aggregation>)", or "incomplete: <n> tests without a score" |
| `session_limit` | `TLE` | "session deadline <d>" |
| `idle` | `IDLE` | "no successful forwarding observed for <N> s" |

Qualifiers of judged tests: `OK`, `WA` and `PE` name the
deciding judge ("checker", "interactor") and the head of its message
(§4.6; a `simple` checker's stderr); `CHECKER-FAILED` names the class and
role ("checker crashed: signal 11", "interactor: report invalid
(disagreement)", "validator: invalid input") instead of "invalid answer";
`UNCHECKED` says "cleanup not verified: report not read" for
`checking_unverified`; `TLE` of an interactive test decided by
`PeerAllowanceExpired` says "did not exit after the interactor finished";
`CE` of a role build says "<role> build" and its cause. A judge's message
is the judge's text, quoted and sanitized like every line (§8); it is never
a summary template.

Qualifiers on a problem with `io.output`: `WA` after
`output_rejected` says "no output file <name>", "output file <name> not
found under that exact spelling", "output file <name> is a link"
(respectively "a directory", "not a regular file") "output file <name>
not readable" or "too many files in the working directory to find
<name>"; `OLE` with stream `output_file` says "output file <name>:
more than <budget> bytes"; a diagnostic collection check after another
label is shown as "(no output file)" or the same texts in parentheses.
`OK` and `WA` after a comparison name the output file instead of stdout.
The job summary of such a problem adds "(file I/O)".

**Display profiles** (checkers design §6.6), selected by
`[check] display` (C §9.1.1) and recorded in `target.check.display`: `cpf`
(default) uses the labels above; `judge-cf` shows `presentation_error` with
the label `WA` (qualifier still "presentation error"). A display profile
changes human labels only: the stored verdict, `by_verdict`, the exit
status and every document keep the raw token.

The label set is exhaustive. Summary templates (design §6.1), each fixed
and table-tested (§9.5):

- Job summary: "<passed>/<selected> passed (<comparator> comparison)";
  "<passed>/<selected> passed (<profile> checker)",
  "<passed>/<selected> passed (interactive, checking <checking>)", and on a
  points problem "<value>/<max> points (<aggregation>) over <selected>
  tests"; a validator adds "(inputs validated)". No template contains
  "accepted" or "correct".
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
  final (§§2.4–2.5; a judged test once all its records
  are final, §§2.11–2.12). Intent stops admission and arbitrates checks (E §6.4).
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
- **A Submission publication** (`submit` jobs, in
  place of fact publication; submission contract §6.7). Eligible iff
  `primary_verdict` is `passed`, `succeeded`, or `scored` with a full
  aggregate; otherwise `not_attempted{not_eligible}`. The cancellation
  cutoff is the fact cutoff above; the commit points are the version
  publish and the `current` replace; intent after the cutoff never
  prevents or undoes them. The clipboard follows publication in this
  phase.
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
  | after V, until the end of A (`test` and `submit`) | `fact` | verdict frozen; fact (`submit`: submission publication) per the cutoff |
  | after A, until the end of H (`test` only) | `hooks` | running hook cancelled, later hooks skipped |
  | after the last of V, A, H, until the end of W | `settlement` | settlement still completes (bounded); nothing else changes |
  | after W, until the F lock is taken (phase E included) | `closure` | nothing changes |

  For `build`, `run`, `cmd` and `hook` jobs (no A, H) the interval after V
  belongs to `settlement` and then `closure`; for a `submit` job (A, no H)
  the interval after A does. Every phase other than
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

A `stress` job runs the phases P, V, W, E, F, S, D (no
fact, no hooks). Its phase P contains the builds and every iteration
including its retirement (stress contract §6.3, saved-failure promotion
included); W then settles the builds' artefacts (iteration artefacts
settle at each retirement, E §6.2). Cancellation phases follow the table
above with `settlement` and `closure` after V.

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

### 2.11 Batch judged tests

A batch judged test has, contiguous in plan order, the executions v (its
validator run, if a validator is declared), s (its solution run) and c (its
checker run, kind `checker`). Admission follows §2.1 (each run admitted
only after the earlier one's record is final; `skipped{input_not_validated}`
after a validator that did not report the input valid; c
`skipped{not_eligible}` when s has no record, coverage `not_started` or
`transferred`, or class `infrastructure` or `cancelled`) and the
cancellation arbitration of E §6.4: intent set before a judge run's
admission skips it (`skipped{cancelled}`), intent set while it runs stops
it (class `cancelled`), intent set after it finished changes nothing. A
checker admitted after an s class other than `exited_zero` is
**diagnostic** (`diagnostic: true`): its outcome is evidence and never
decides, like a diagnostic comparison (§2.4).

The test is derived once, after the last of its records is final, under
the verdict lock: the **first** matching row gives the check and the test
verdict. Judge outcomes are read by the profiles of E §2.1; "class" is the
judge run's own §2.2 class.

| # | Condition | Check | Test verdict |
|---|---|---|---|
| B1 | v `skipped{cancelled}` | `not_admitted{cancelled}` | `cancelled` |
| B2 | v `skipped{build_not_succeeded \| admission_closed \| submit_not_succeeded}` | `not_admitted{run_not_eligible}` | `not_run{reason}` |
| B3 | v `not_started{≠ cancelled_before_start}`, or class `infrastructure` | `not_admitted{run_not_eligible}` | `infrastructure` |
| B4 | v `not_started{cancelled_before_start}`, or class `cancelled` | `not_admitted{run_not_eligible}` | `cancelled` |
| B5 | v profile `testlib`, class other than `exited_zero` | `judge_failed{test_data_error, validator, validation_failed}` | `checker_failed` |
| B6 | v profile `simple`, class `nonzero_exit{1}` | `judge_failed{test_data_error, validator, invalid_input}` | `checker_failed` |
| B7 | v profile `simple`, any other class except `exited_zero` | `judge_failed{crashed, validator, <class reason>}` | `checker_failed` |
| B8 | kind `compare` (v `exited_zero`) | §2.4 and §2.5 over s, unchanged | §2.5 |
| B9 | s `skipped{cancelled}` | `not_admitted{cancelled}` | `cancelled` |
| B10 | s `skipped{build_not_succeeded \| admission_closed \| submit_not_succeeded}` | `not_admitted{run_not_eligible}` | `not_run{reason}` |
| B11 | s `not_started{≠ cancelled_before_start}`, or class `infrastructure` | `not_admitted{run_not_eligible}` | `infrastructure` |
| B12 | s `not_started{cancelled_before_start}`, `transferred`, or class `cancelled` | `not_admitted{run_not_eligible}` | `cancelled` |
| B13 | c `skipped{cancelled}` | `not_admitted{cancelled}` | `cancelled` |
| B14 | s `exited_zero`; c `skipped{admission_closed}` | `not_admitted{run_not_eligible}` | `not_run{admission_closed}` |
| B14a | s `exited_zero` (test class, §2.15); collection not `collected`; c `skipped{not_eligible}` | the collection check of §2.15: `output_rejected{reason}`, or `check_error{os_error}` for `not_collected` | `wrong_answer`, respectively `infrastructure` |
| B15 | c `not_started{cancelled_before_start}`, `transferred`, or class `cancelled` | `unchecked{checking_interrupted}` | `cancelled` |
| B16 | s class `wall_deadline`, `output_budget`, `signal` or `nonzero_exit` | c's outcome by B18–B37 with `diagnostic: true`, or `not_admitted{run_not_eligible}` when c is `skipped{admission_closed}`, `not_started` or `infrastructure`; when the output file was not collected (c `skipped{not_eligible}`), the §2.15 collection check with `diagnostic: true` | the s class (the test class) |
| B17 | s `exited_zero`; c `not_started{≠ cancelled_before_start}` or class `infrastructure` | `not_admitted{run_not_eligible}` | `infrastructure` |
| B18 | `simple` c, `exited_zero`, cleanup not `unverified` | `passed` | `passed` |
| B19 | `simple` c, `nonzero_exit{1}`, cleanup not `unverified` | `rejected{wrong_answer, checker}` | `wrong_answer` |
| B20 | `simple` c, any other class | `judge_failed{crashed, checker, <class reason>}` | `checker_failed` |
| B21 | `testlib` c, stage 1: class `signal`, `wall_deadline` or `output_budget` | `judge_failed{crashed, checker, <class reason>}` | `checker_failed` |
| B22 | c of either profile, cleanup `unverified` (after B20–B21: a crash or own limit decides first) | `unchecked{checking_unverified}` | `unchecked` |
| B23 | `testlib` c, the report read fails with an OS error E §2.1 does not classify as `ReportInvalid`, or its extraction fails (reservation refused, I/O error) | `check_error{os_error}` | `infrastructure` |
| B24 | stage 2: report missing, a link, not a regular file, not readable (permission) | `judge_failed{report_invalid, checker, report_missing \| report_not_regular \| report_unreadable}` | `checker_failed` |
| B25 | stage 2: report larger than 64 KiB | `judge_failed{report_invalid, checker, report_too_large}` | `checker_failed` |
| B26 | stage 2: undecodable (an undeclared report that is not UTF-8, an unsupported declared encoding, bytes invalid in the declared one) | `judge_failed{report_invalid, checker, report_undecodable}` | `checker_failed` |
| B27 | stage 3: not well-formed, or forbidden markup | `judge_failed{report_invalid, checker, report_malformed}` | `checker_failed` |
| B28 | stage 4: element, outcome or attribute unknown or misplaced | `judge_failed{report_invalid, checker, report_schema}` | `checker_failed` |
| B29 | stage 4: `points` outside the grammar or the range 0 ≤ raw ≤ scale | `judge_failed{report_invalid, checker, points_invalid}` | `checker_failed` |
| B30 | stage 5: exit code not 0, 1, 2, 3 or 7 (checked before the outcome, so an unsupported exit with an unsupported outcome is this row) | `judge_failed{protocol_unsupported, checker, exit_unsupported}` | `checker_failed` |
| B31 | stage 5: supported exit code, outcome unsupported (`partially-correct`, another known outcome, `points` under `pass_fail`) | `judge_failed{protocol_unsupported, checker, outcome_unsupported \| points_not_enabled}` | `checker_failed` |
| B32 | stage 6: a supported pair outside the matrix (e.g. exit 0 + `wrong-answer`) | `judge_failed{report_invalid, checker, disagreement}` | `checker_failed` |
| B33 | (0, `accepted`) | `passed` | `passed` |
| B34 | (1, `wrong-answer`) | `rejected{wrong_answer, checker}` | `wrong_answer` |
| B35 | (2, `presentation-error`) | `rejected{presentation_error, checker}` | `presentation_error` |
| B36 | (3, `fail`) | `judge_failed{declared_fail, checker, declared_fail}` | `checker_failed` |
| B37 | (7, `points`) | `scored{raw, scale, checker}` | `scored` |

- Rows B1–B7 decide on v; rows B8–B37 apply only when there is no v or v
  was `exited_zero` (input valid); rows B13–B37 only with kind `checker`. With an answer that is
  deliberately none under `answer = "none"` the judge received an empty
  file and the rows apply unchanged; `answer` stays `absent` in §4.6.
- `<class reason>` is the judge run's class token (`signal`,
  `wall_deadline`, `output_budget`) or `exit_code` for a `nonzero_exit`.
- Every combination not listed (for example c `skipped{not_eligible}`
  after an s with class `exited_zero` whose output was collected or that
  has no `io.output`, or `skipped{input_not_validated}` after a valid
  input) is an orphan: check and test verdict
  `internal_error{invariant}` (§2).
- A checker after a solution failure never yields success (B16); a
  checker failure is never `wrong_answer` and never a zero score (B20, B21,
  B24–B32, B36; §2.13).
- Compatibility (compare spec §8): kind `compare` keeps the comparator's
  outcomes exactly (B8); a comparator's invalid answer stays
  `checker_failed{test_data_error, invalid_answer}` (§2.4).

### 2.12 Interactive tests

An interactive test has the executions v (if a validator is declared), s
and i (one session, admitted atomically, E §9a) and c (the follow-up
checker, checking `checker` or `interactor+checker`). Rows B1–B7 decide
first. Then:

| # | Condition | Test verdict |
|---|---|---|
| I1 | s and i `skipped{cancelled}` | `cancelled` |
| I2 | s and i `skipped{build_not_succeeded \| admission_closed \| submit_not_succeeded}` | `not_run{reason}` (`input_not_validated` never reaches this row: B3–B7 decide first) |
| I3 | **cancellation snapshot**: s and i `transferred` (cancellation during committed pair creation, E §3.4; the record holds `CancelRequested` and no start facts), or the follow-up checker `transferred` | `cancelled`, without reduction (below) |
| I4 | otherwise (the pair was admitted) | the session verdict of `reduce(record, checking, scoring)`, policy `interactive_v7` (checkers design §6.2), over the closed session record (checkers design §6.1), mapped below |

| Session verdict | Test verdict | Score (§2.13) |
|---|---|---|
| OK (`AuthorityComplete{Passed}`) | `passed` | scale |
| WA / PE (I's valid rejection, or `AuthorityComplete{Failed}`) | `wrong_answer` / `presentation_error` | 0 |
| score (`AuthorityComplete{Scored}`) | `scored` | raw |
| TLE (`StopRequested{S, OwnLimit(Wall)}` or `{S, PeerAllowanceExpired}`) | `wall_deadline` | 0 |
| OLE (`StopRequested{S, OwnLimit(DialogueOutput)}`) | `output_budget` | 0 |
| RE (S's natural nonzero exit or signal, not normalised) | `signal` or `nonzero_exit`, by S's `RootExited` status | 0 |
| `session_limit` | `session_limit` | 0 |
| `idle` | `idle` | 0 |
| `CheckerFailed{class}` | `checker_failed` | none |
| `Unchecked{CheckingUnverified}` | `unchecked` | none |
| `cancelled` | `cancelled` | none |
| `infrastructure` | `infrastructure` | none |
| `internal_policy_error` | `internal_error{internal_policy_error}`: an internal failure before V (§2), `result.internal` locus `test` | none |

- **Cancellation snapshots** (I3) are not closed records, so they never
  reach the reducer or its step 0: the session member has `verdict`
  `cancelled`, `decided_by` `{"kind":"transferred","role":token}` (the
  first transferred role: `solution` for the pair, else `checker`) and
  `facts` as recorded; s and i (or c) have coverage `transferred` and class
  `cancelled` (§4.5 creation cancellation); a follow-up checker never
  started after a transferred pair is `skipped{cancelled}`; `judges[]`
  entries of transferred runs have `result: null`; no score. A transferred
  follow-up checker can follow only an eligible record (checkers design
  §6.1 item 6: no decisive fact, no cancellation, no infrastructure), so
  `cancelled` is the verdict step 4 gives a cancellation before execution
  completes. The later cleanup ticket outcome changes nothing (E §3.4).
- The test's `class` is s's §2.2 class, as evidence only (a teardown stop
  shows as `signal`); `check` is `null` and `diagnostic` `false`; the
  decision is in `session` (§4.6). `CheckingInterrupted` is unreachable
  (checkers design §6.2 completeness) and never written for a session.
- Coverage of c: `Started` → `started`, `NotStarted` → `not_started`,
  `CheckerSkipped{NotEligible}` → `skipped{not_eligible}`,
  `CheckerSkipped{Cancelled}` → `skipped{cancelled}`; in mode
  `interactor` no c is planned.
- **Prerequisites of the reducer**, each fixed by checkers design §6.1–6.2
  and tested in §9.6: the record is lossless and distinct from the lossy
  event transport; it is closed (every applicable emission item has
  happened) before reduction, and step 0 rejects any record violating the
  emission contract, its consistency rules, the `Stopped(n)` reference,
  the cancel-before-start order, distinct decisive positions or the bound
  (`internal_policy_error`, never success); positions are `decision_pos`,
  so a report read after its judge exited is attributed to that exit;
  facts of one wakeup are ordered by the same-wakeup rule; **authority
  completion** is the only path to a success or a score; **peer
  shutdown** (peer allowance, teardown stops) is decisive only for the
  late role's own limit, never through teardown; **cleanup uncertainty**
  is `NotRead` (`Incomplete`: infrastructure) or `Unverified` (unchecked)
  for a judge, and for S follows E §6.4 (`Unverified` does not block, a
  note only).

### 2.13 Scores

On a problem with a points `scoring` (C §9.1.5):

- **Per-test score** (`tests[].score`), from the test verdict: `passed` →
  scale; `scored` → raw; `wrong_answer`, `presentation_error` → 0;
  `wall_deadline`, `session_limit`, `idle`, `output_budget`, `signal`,
  `nonzero_exit` → 0; every other verdict (`checker_failed`, `unchecked`,
  `cancelled`, `infrastructure`, `not_run`, `internal_error`) → no score
  (`null`). A diagnostic check never gives a score. Without a points
  `scoring` every `score` is `null`.
- **Values** are `i128` fixed point in units of 10⁻⁹ (checkers design §4),
  written as `score` (§3.2). raw keeps the received digits' value; more
  than 9 fractional digits never reaches here (stage 4, B29).
- **Aggregate** (`result.score`), computed at V over the job's selected
  tests: zero tests → `no_tests`; any selected test without a score →
  `incomplete{missing}` (the count); otherwise `points{value}` with value
  = Σ scores (`sum`) or the minimum (`min`), `max` = number of tests ×
  scale (`sum`) or scale (`min`), and `full` iff value = max. Never a
  number when incomplete; a score is final only once its test's authority
  completed (checkers design §6.1).
- `incomplete` always comes with a job verdict of a higher §2.7 row (each
  score-less test verdict has its own row), so it never decides the exit
  status alone; `scored` exits 0 iff `full` (§5.2).
- With `scoring = "pass_fail"` a judge's points are
  `ProtocolUnsupported` (B31; for a session, checkers design §6.3) and
  `result.score` is `null`.

### 2.14 Stress jobs

Iteration classes are derived by stress contract §4 (batch rows S1–S8,
interactive rows T1–T7) from the iteration's records, using §2.2 classes,
§2.4 comparisons, §2.11 check rows and the §2.12 session verdicts. They
are folded into `stress` (§4.14) and never become `tests[]` entries.

`result.primary_verdict` of a `stress` job, at V, first matching value:

1. `internal_error`: an internal failure recorded before V (§2), including
   an iteration orphan and an `internal_policy_error` session (stress
   contract §4).
2. `infrastructure`: a build result `infrastructure`, or a retired
   iteration of class `infrastructure`.
3. `cancelled`: a build result `cancelled` (or a build coverage cancelled,
   §2.7 row 3), a retired iteration of class `cancelled`, or the stop
   reason `cancelled` (intent prevented an iteration the budget allowed).
   Intent set after the stop reason was already decided cancels nothing:
   the completed verdict stands and the intent is recorded (§2.7).
4. `build_failed`: a build result `failed`.
5. `stress_cleanup_failed`: the stop reason `cleanup_failed` (stress
   contract §6.4).
6. `candidate_failure`, 7. `judge_failure`, 8. `oracle_failure`,
   9. `validator_failure`, 10. `generator_failure`: the first of these
   classes, in this order, among retired iterations (a solution failure
   first, as test verdicts precede `checker_failed` in §2.7).
11. `no_iterations`: no iteration retired (only reachable by the
    `counter_limit` stop before k = 0).
12. `passed`: otherwise.

- A replay job (mode `replay` or `replay_binaries`) uses the same rows over
  its single iteration; `stress.replay.reproduced` never changes the
  verdict or status.
- `result.partial` is true iff a build is not `started`; iterations not
  admitted because a stop held are not partial coverage.
- `result.counts` counts the builds (primary population) and zero tests;
  `by_verdict` is all zero. Iteration counts are in `stress`.

### 2.15 File-based I/O

A problem that declares `[config] io` (C §6.2) is batch, never
interactive, so §2.12 is unchanged. `io.input` alone changes no
derivation (the solution's stdin is null, E §5.6; `target.io` records
it). With `io.output`, each test's solution run s has a **collection
outcome** (E §5.6), reported as `tests[].output_file` (§4.6), which enters
the derivations of §§2.4, 2.5 and 2.11 in three ways:

1. **Test class.** `tests[].class` is the **test class**: s's run class
   (§2.2), except that a `too_large` collection raises a class
   `exited_zero`, `nonzero_exit` or `signal` (each below `output_budget`
   in E §6.4) to `{"kind":"output_budget","stream":…,"streams":[…]}`,
   with `output_file` appended to `streams` after any stdout or stderr
   breach and as `stream` when it is the only one. A class
   `output_budget` (a `Kill` breach of stdout or stderr) keeps its
   `stream` and gets `output_file` appended; `wall_deadline` is
   unchanged. Every rule of §§2.4, 2.5 and 2.11 that reads s's class reads
   the test class. `executions[].summary.class` stays the run class of the
   record: a test can be `output_budget` while its execution entry says
   `exited_zero`, and the report says so (`output_file.kind` is
   `too_large`).
2. **Collection check.** When s is eligible (§2.4 steps 1–2) and the check
   is admitted under the cancellation arbitration (§2.4 step 3), a
   collection other than `collected` decides the check without a
   comparison and without a checker (for kind `checker`, c is
   `skipped{not_eligible}`, §2.1, and B14a or B16 applies):

   | Collection (`output_file.kind`) | Check | Test verdict after test class `exited_zero` |
   |---|---|---|
   | `missing` | `{"kind":"output_rejected","reason":"output_missing"}` | `wrong_answer` |
   | `misnamed` | `output_rejected{output_misnamed}` | `wrong_answer` |
   | `not_regular` (`link`, `directory`, `other`) | `output_rejected{output_not_regular}` | `wrong_answer` |
   | `unreadable` | `output_rejected{output_unreadable}` | `wrong_answer` |
   | `too_many_entries` (more than 65 536 entries before the name, E §5.6) | `output_rejected{output_dir_too_large}` | `wrong_answer` |
   | `too_large` | `output_rejected{output_too_large}` | – (the test class is `output_budget`) |
   | `not_collected` (`io`, `timeout`, `unstable`) | `check_error{os_error}` (`os_error` `null` except for `io`) | `infrastructure` |

   Settled: a missing or rejected output file is `wrong_answer` with the
   `output_rejected` reason (never presentation error, never a label of
   its own).

   After any other test class the check is **diagnostic** (`diagnostic:
   true`) and the test class decides (§2.5, B16). The check is decided
   before the answer state, so a missing output file is `wrong_answer`
   also for a test without an answer (never `unchecked{no_answer}`): the
   failure is known without one. A program that writes no output file
   never passes, whatever the answer, the comparator or the checker.
3. **Judged output.** After `collected`, the comparator's O is the
   extracted file (compare spec §1.1) and the checker's `output` is a copy
   of it (E §2.1); stdout is evidence only (`executions[].summary.stdout`,
   `<run>/out/stdout`). A `not_attempted` collection (s not eligible) adds
   nothing: the existing rows decide.

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
| `irid` | iteration run id `<iteration>.<role>`: the iteration a `u64` in decimal without leading zero, `.`, a stress contract §3.1 run-role token | 43 |
| `fid` | saved-failure id (stress contract §7.1) | 78 |
| `jid` | job id (S §6.2) | 36 |
| `ident` | Identifier (S §2.1, ≤ 64) | 66 |
| `pident` | problem identity: `local:` + Identifier, or a remote identity `<site>:<native>` (S §2.1; ≤ 64 bytes, since its directory name `<site>-<native>` is an Identifier) | 72 |
| `ver` | crate version, ≤ 32 ASCII | 34 |
| `rel` | job- or workspace-relative path built from the ids above; maximum stated per member | per member |
| `text` | cpf-generated text, serialized ≤ 260 bytes including quotes (the writer cuts at a character boundary and ends with `…`); informative | 260 |
| `dec` | a TOML float as `{"text": shortest round-trip, "bits": 16 hex}` (compare spec §6 `F`) | 69 |
| `ns` | full native value (§3.3), n source bytes | max(2n + 11, 4⌈n/3⌉ + 16) |
| `nsum(P)` | bounded native summary (§3.3) | 132 + max(2P + 11, 4⌈P/3⌉ + 16), the `ns` bound of its head; this equals 2P + 143 for P = 9 and every P ≥ 11 (every production P in this contract is ≥ 128; fixtures may use smaller P, covered by the general formula) and exceeds it for P ≤ 8 and P = 10 (e.g. 156 at P = 4); rows below computed with the earlier 2P + 149 keep 6 B of slack per summary |
| `bytes` | `{"bytes_b64": …}` of n bytes | 4⌈n/3⌉ + 17 |
| `score` | a fixed-point value in units of 10⁻⁹ (§2.13) as a JSON string: the integer part in minimal decimal (`"0"` when zero), then, only if the value has a fraction, `.` and 1–9 digits without a trailing zero; no sign, no exponent. ≤ 14 integer digits (10 000 tests × 10⁹) | 26 |

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
- In a `stress` job only builds are admitted executions
  with run ids (≤ 6). Iteration runs are identified by `irid` (§3.2),
  which is disjoint from `rid` (it contains `.`), unbounded in its
  iteration part and used as the run's directory name under
  `iterations/` (S §6.1).

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
  may change in place; it has been reconciled once with every gate-2
  addition, and Appendix A lists its complete content. After that
  reconciliation no contract adds a schema-1 enum value or changes a
  meaning within schema 1 (members may still be added, as above). A reader meeting a newer `schema` uses only `schema`,
  `document`, `platform` (S §9: raw display only); cpf never prunes a job
  directory whose `report.json` is newer.

### 3.7 Size bounds

Every bound is a sum of serialized maxima (§3.2) over the actual variant
schemas of §4, taking the maximum over mutually exclusive variants and
including escaping and base64. §9.4 serializes the worst case of each item
to check the arithmetic. Sizes in bytes. Every row's sum is recomputed from
the rows above it with the object and list costs of §3.2; where a row was
first computed with a larger leaf value (`nsum(P)` at 2P + 149, §3.2) the
stated sum keeps that slack.

#### 3.7.1 Items

| Item | Composition (worst variant) | Sum | Bound |
|---|---|---|---|
| `Cause` | `capture_loss`: kind, stream, loss tokens | 129 | |
| class | `infrastructure`: kind + 8 × 129 + `causes_dropped` | 1 122 | |
| class (not infrastructure) | `signal`: kind, `i32`, name token, bool | 169 | |
| test class `output_budget` with three streams (§2.15) | kind, stream, `streams` [3 × token] | 204 | |
| `execution` (summary) | `started`: kind, status 108, trigger 87, `dur`, 3 × `Violation` 119, cleanup 87, `u32` | 804 | |
| stream capture | 3 `u64` + `exceeded_at`, 2 bool, 6 × `Loss` 66, `file` 19 | 608 | |
| `Measurement` | 7 tokens + `u64` | 346 | |
| `InstalledLimit` | 2 tokens, 6 `u64`, `outer` 74 | 360 | |
| **`RunSummary`** | class 1 122 + execution 804 + timing 114 + deadline 22 + 2 × 608 + 5 × 346 + 2 × 360 + 8 tokens + 16 tokens + dropped + `u64` + keys | 6 774 | **7 KiB** |
| `Ref` | digest 66, path 20, `PersistError` 337 (`io`: token, `i32`, text) | 451 | |
| **`executions[]` entry** | index, stage, role 162, coverage 160, run 8, program `nsum(256)` 655, 2 × `Ref`, max(summary 6 774, class 1 122) (mutually exclusive), `class` key with `null` 13, invariant 47 | 8 851 | **9 KiB** |
| `tests[]` entry, comparison only | max of: non-infrastructure class 169 + check with `diag` ≤ 2 048 (2 868); infrastructure class 1 122 + small check (1 723) | 2 868 | |
| **`aggregates[]` entry** | test 66, 2 × `u32`, three `dur` | 224 | **256** |
| warning | max of: `recovery` (kind, `u32`, `nsum(128)` 399, text) 741; `persist` (kind, token, rid, `PersistError` 337, text) 721, with an `irid` instead of the rid (stress jobs) 756; every other kind smaller (`clipboard` 634, `retention` with an `irid` 416, `judge_scratch` with an `irid` 389, `io_scratch` 351) | 741; 756 in a `stress` job | **768** |
| hook entry | name 66, trigger, `u32`, outcome (kind + one token) | 244 | |
| capture (header) | check 195 + 2 × (16 × `nsum(128)` + dropped) + counts + `u64` + digest + manifest `rel` 28 | 13 311 | |
| build (header) | max of: `succeeded` + 16 artifacts × 399 (6 574); `failed{missing_output}` with 16 names × 399 and no artifacts (6 664) | 6 664 | |
| base header | job 225, workspace `nsum(1024)` 2 191, problem 509, target 1 587, capture 13 311, fingerprints, build 6 664, result 1 784, fact 453, 32 × hook 244, 24 × warning 741, events, timing, persistence 448, scalars, keys | 53 645 | |
| `Diagnostic` | file `nsum(256)` 655, key text, 2 × `u32`, message text | 1 237 | |
| `output_file` (`tests[]` member, key and comma) | `collected`: kind, `u64`, `hex64`, path 19 (`too_large` 83, others smaller); `null` 19 | 169 | |
| `target.io` (key and comma) | two distinct names of ≤ 255 bytes (never escaped, C §6.2), `stdin` `null_device`, keys | 562 | |
| message | `nsum(4096)` | 8 341 | |
| diagnostic | `nsum(1024)` | 2 197 | |
| **`judges[]` entry** | role, `u32`, profile, result 130 (`judge_failed`: kind, class, reason), message 8 341, stderr 2 197, report `{path, bytes, encoding}` 100, keys | 10 928 | **11 KiB** |
| **`validation`** | `u32`, profile, outcome, stderr 2 197, keys | 2 321 | **2.5 KiB** |
| session facts | ≤ 17 facts (32 reserved), each ≤ 512 bytes plus text ≤ 256 bytes after escaping (checkers design §6.1) | 13 056 | **16 KiB** (16 384) |
| **`session`** | policy, checking, mode, verdict 172, `decided_by`, facts 16 384, deadlines, 2 × direction 353, transcript 347, seeds, 2 bool, timing, idle 243, rerun, keys | 18 842 | **19 KiB** |
| `tests[]` entry base | the comparison-only entry 2 868, its class widened to 204 (+ 35), `score` set (13 + 22), `validation`, `judges`, `session`, `output_file` null or empty (18 + 12 + 15 + 19) | 3 002 | **3 KiB** |
| **`tests[]` entry** of a shape | base 3 002 + v × `validation` + j × `judges[]` entry + s × `session` + o × (`output_file` 169 − 19) | | **T = 3 KiB + v × 2.5 KiB + j × 11 KiB + s × 19 KiB + o × 192** |
| **`roles[]` entry** | role, profile, origin 709, build 6 664, capture 13 355 (§4.2 capture plus `content_sha256`), provenance 594, keys | 21 454 | **21 KiB** |
| judging header | base header 53 645 + `roles` 3 × 21 454 + `target.check` 630 + `fingerprints.judge` + `result.score` 173 + 4 `by_verdict` members, keys | 119 044 | |
| **`Submission`** | submission contract §9.2 (16 file entries of 521 dominate) | 16 942 | **17 KiB** (17 408) |
| `StressDiag` | iteration, seed, role, reason, source, text `nsum(1024)` | 2 401 | |
| class aggregate | class, count, `first` 66, 16 × `StressDiag` + dropped | 38 631 | |
| failure run | role, class 1 122, record `Ref` (path ≤ 64) 497 | 1 681 | |
| failure ref | `fid`, iteration, seed, 3 tokens, `saved` 433 (`not_promoted` with path ≤ 85 and text), 8 × failure run | 14 186 | |
| **`stress` member** | 2 tokens, seeds 62, budget 70, stop 80, iterations 144, 3 `u64`, 8 class aggregates 309 057, 20 failure refs 283 741, `failures_dropped`, 8 role timings 1 401, replay 192, `cleanup_failed` (16 × `nsum(256)` + dropped) 10 630, keys | 605 717 | **592 KiB** (606 208) |
| **`stress_failures` entry** | name `nsum(256)`, state, detail text, `fid`, 4 tokens, iteration, seed, time, binaries | 1 391 | **1.5 KiB** |

#### 3.7.2 Bound per document kind

| Document kind | Derivation | Bound |
|---|---|---|
| `job_report`, kinds `build`, `test`, `run`, `cmd`, `hook` | header H: judging header 119 044 + `target.io` 562 + the `null` members `stress` (14) and `submission` (18) = 119 638 ≤ 131 072; then Σ per planned execution (entry + comma ≤ 9 KiB) + Σ per test (T + comma) + Σ per aggregate (≤ 256) | **B = 128 KiB + 9 KiB × N + T × N_t + 256 × N_a** |
| `job_report`, kind `submit` | header H_submit: 119 638 − 4 (`submission` no longer `null`) + `Submission` 16 942 = 136 576 ≤ 148 480; the same per-execution, per-test and per-aggregate terms (N counts the probe and the submit stage) | **B = 145 KiB + 9 KiB × N + T × N_t + 256 × N_a** |
| `job_report`, kind `stress` | header: judging header 119 044 + `target.io` `null` 10 + 2 further `roles[]` entries (generator, brute) 2 × (21 454 + 1) + 24 × (756 − 741) for `irid` warnings + `submission` `null` 18 = 162 342 ≤ 160 KiB = 163 840; report: 160 KiB + 6 builds × (9 KiB + 1) + `stress` member 605 717 + key 10 = 824 869 ≤ 1 048 576 | **B_S = 1 MiB** (fixed) |
| `job_report`, refusal (§4.12) | header members a refusal can carry (job, workspace, problem, result, 24 warnings, events, timing, persistence) + refusal (class, error with `nsum(256)`, 64 × 1 237 + dropped) | 103 966 ≤ **104 KiB** |
| `capture_manifest` (§4.2) | exact size computed before it is written | exact |
| `event_stream` | header line: a few tokens and `cpf_version` (< 400); each event line: max of `run_finished` (summary + 99) 6 873, `iteration_run_finished` (summary + 124) 6 898, `output_chunk` (4 096 bytes → 5 464 base64 + 157) 5 621, with an `irid` 5 656, others ≤ 400; final envelope: the embedded document's bound + 28 (29 with the line feed) | **7 KiB** per line; final envelope B + 28 or B_S + 28 |
| `error` (§4.12) | `schema`, `document`, `platform`, `command`, `class`, `exit`, error (kind, text, `nsum(256)`) 973, 64 × 1 237 + dropped, keys | 80 487 ≤ **80 KiB** (81 920) |
| `submit` (§6.1) | `schema`, `document`, `platform`, `problem` 72, `Submission` 16 942, 24 × warning 741 + dropped, keys | 35 003 ≤ **35 KiB** (35 840) |
| `stress_failures` (§4.15) | `schema`, `document`, `platform`, `problem` 72, 10 000 × 1 391 + 9 999 commas + brackets, `entries_dropped`, keys | 13 920 237 ≤ **14 MiB** (14 680 064) |
| `config_show`, `status`, `ls`, `search`, `notes`, `archive_plan`, `archive`, `restore`, `fetch` | lists workspace-sized and uncapped (the archive documents bounded by the archive contract's limits); written only to stdout, never persisted or reserved | no fixed bound |

N is the number of planned executions (≤ 10 000, C §8.2), N_t the number
of `tests[]` entries (selected tests, ≤ N), N_a the number of
`aggregates[]` entries (≤ N), and T the per-test bound of the job's
**shape**, which all its tests share: v = 1 iff a validator is declared, j
the number of judges per test (0 for kind `compare`, 1 for kind `checker`
or checking `interactor`, 2 for checking `checker` or
`interactor+checker`), s = 1 iff the problem is interactive, o = 1 iff the
problem declares `io.output` and the job applies it (o and s are never
both 1, C §6.2). The comparator value is budgeted at 8 `dec`-sized members
(the float comparator has 6, compare spec §5.1).

Worst cases, each with N at the cap:

- kind `compare`: B = 131 072 + 10 000 × (9 216 + 3 072 + 256) =
  125 571 072 B (119.8 MiB), 2 × B = 251 142 144 B (239.5 MiB);
- kind `compare` with `io.output`: B = 131 072 + 10 000 × (9 216 + 3 264
  + 256) = 127 491 072 B, 2 × B = 254 982 144 B, below 256 MiB =
  268 435 456 B;
- 1 000 interactive tests with a validator and a follow-up checker
  (4 004 executions, T = 3 072 + 2 560 + 2 × 11 264 + 19 456 = 47 616):
  B = 131 072 + 4 004 × 9 216 + 1 000 × 47 616 = 84 647 936 B (80.7 MiB);
  the same plus 5 378 hooks (N = 9 382): 2 × B = 268 423 168 B, 12 288 B
  below 256 MiB; plus 5 379 hooks: 268 441 600 B, refused;
- a `submit` job, kind `compare` with `io.output`, 10 000 executions
  (probe, submit stage, artifact build, 9 997 tests): B = 148 480 +
  10 000 × 9 216 + 9 997 × 3 264 = 124 938 688 B, 2 × B = 249 877 376 B;
- a `stress` job: B_S = 1 048 576 B whatever its iteration count, since
  iteration runs are never listed (§4.14 holds only fixed-size aggregates,
  the first 20 failure refs and the first 16 diagnostics per class); its
  `tests[]` and `aggregates[]` are empty and `executions[]` holds ≤ 6
  builds.

#### 3.7.3 Reservations

Amendment to E §5.4/§6.2 and S §6.4, §10. Before the first run the job
reserves, through the S §6.4 interface, the **report reservation**: exactly
2 × the report bound of its plan (the bound for `report.json` plus the same
again for its temporary file during replacement): 2 × B for the job kinds
`build`, `test`, `run`, `cmd`, `hook` and `submit`, with the header of its
kind, and the fixed 2 × B_S = 2 MiB for a `stress` job. It is taken once
before the first run and held until report publication settles (E §5.4).
It is never taken lazily: a reservation made at finalization could find
the job budget consumed by runs, captures and judge staging, and
final-report headroom would no longer be guaranteed.

A plan with 2 × B > 256 MiB is a plan error (C §8.2; for a `submit` job,
submission contract §6.2); with the judged-test cap of 1 000 this is
reachable only through planned hooks, and every admitted plan is
≤ 256 MiB by the cap (worst cases above).

The other job reservations are separate and additive: the capture manifest
(its exact size plus its temporary, reserved before it is written during
freezing), frozen copies and captured files (S §6.4), each run's stream
and metadata reservations, judge staging and session transcripts, the
staged input and extracted output file of file-based I/O runs (E §5.4,
§5.6), the artifact of a `submit` job (submission contract §6.4) and a
stress job's iteration resources (stress contract §6.6). At the first
run's admission the job's reserved bytes are therefore the report
reservation plus that run's own reservations; the capture manifest is
already retained.

## 4. Report schema (`job_report`, schema 1)

### 4.1 Top level

| Member | Type | Content |
|---|---|---|
| `schema` | `u32` | `1` |
| `document` | `token` | `job_report` |
| `platform` | `token` | `unix`, `windows` |
| `outcome` | `token` | `finished` (a ready job ran) or `refused` (§4.12) |
| `cpf_version` | `ver` | |
| `command` | `token` | `build`, `test`, `run`, `cmd`, `hook`; `stress`; `submit_prep` |
| `job` | object? | §4.2; `null` only for a refusal before registration |
| `workspace` | `nsum(1024)?` | workspace root as cpf located it |
| `problem` | object? | §4.2 |
| `target` | object? | §4.3 |
| `capture` | object? | §4.2 |
| `fingerprints` | object? | `{source: hex64, tests: hex64, judge: hex64?}` once run facts compute them (storage contract §6.5); `null` until then. `judge` is the judge fingerprint (C §9.1.3), `null` for a problem without a role |
| `build` | object? | §4.4 |
| `roles` | `[Role; ≤5]` | §4.4, order checker, validator, interactor; then generator, brute (stress jobs only); empty without roles |
| `executions` | `[Execution; ≤10000]` | §4.5, plan order |
| `tests` | `[Test; ≤10000]` | §4.6, frozen-manifest order |
| `aggregates` | `[Aggregate; ≤10000]` | §4.7; empty unless `cmd` |
| `result` | object | §4.8 |
| `fact` | object? | §4.9 |
| `submission` | `Submission?` | submission contract §9.2; `null` unless a `submit` job (also for its refusal) |
| `hooks` | `[Hook; 32]` | §4.9, plan order, first 32 |
| `warnings` | `[Warning; 24]` | §4.10, first 24 in occurrence order, subject to the late-cancellation reservation |
| `events` | object | `{dropped: u64}` (§7.4; `"0"` without a consumer) |
| `timing` | object | `{started_at: time, finished_at: time, duration_us: dur}`: admission start (S §6.2 step 1) to F |
| `refusal` | object? | §4.12 |
| `stress` | object? | §4.14; `null` unless `command` is `stress` |
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
    "lists_sha256":hex64,"roles":[{"role":token,"files":[ns…]}]}` holding
    the complete lists with full values (`roles`: each
    role's captured files in capture order, C §9.1.3; empty without roles).
    It
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

- `check` (member after `compare`; `CheckConfig?`, `null`
  for a job without checks): `{kind: token, answer: token, scoring:
  Scoring, display: token, interactive: Interactive?}` with `kind`
  `compare`, `checker`, `interactive`; `answer` `file`, `none`; `display`
  `cpf`, `judge_cf`; `Scoring`: `{"kind":"pass_fail"}` or
  `{"kind":"points","scale":score,"aggregation":token}` (`sum`, `min`);
  `Interactive` (`null` unless interactive): `{checking: token, mode:
  token, idle_us: dur?, early_reader_close: bool, interactor_allowance_us:
  dur, peer_allowance_us: dur, solution_seed: token, interactor_seed:
  token}`, `checking` `interactor`, `checker`, `interactor_checker`, `mode`
  `relay`, `direct`, `idle_us` `null` when the watchdog is off, seeds
  `none`, `env`, `argv`. A test job always has a non-null `check` (kind
  `compare` by default).
- `io` (member after `check`; object?): the file-based
  I/O **applied** by this job, `{input: string?, output: string?, stdin:
  token}`: the declared names exactly as configured (valid Unicode without
  control characters, C §6.2; a missing field is `null`) and the
  solution's stdin, `null_device` with `io.input`, `test_input` otherwise
  (E §5.6). Non-null only in `test` jobs of a problem with `[config] io`;
  `null` in every other job (`build`, `run`, `cmd`, `hook`), which ignore
  `io` (C §6.2), even when the problem declares it. `cpf config show`
  reports the declaration itself.
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
- `BuildResult`: `{"kind":"skipped","reason":token}`
  (`build_not_succeeded`, `admission_closed`; role builds only, §2.3;
  `submit_not_succeeded`, the artifact build and role
  builds of a `submit` job),
  `{"kind":"succeeded"}`, `{"kind":"cancelled"}`,
  `{"kind":"infrastructure","causes":[Cause; 8],"causes_dropped":u32}`,
  `{"kind":"failed","cause":BuildCause}`, `{"kind":"internal_error",
  "invariant":token}`. `BuildCause`: a class object (§4.5) other than
  `infrastructure`/`cancelled`, or `{"kind":"missing_output","names":
  [nsum(128); 16],"names_dropped":u32}`,
  `{"kind":"generated_program_invalid","name":nsum(128),"reason":text}`,
  `{"kind":"output_too_large","bytes":u64,"cap":u64}`.
- `artifacts`: the validated artifact set (first 16) for `succeeded`; empty
  otherwise.

**Roles.** `roles[]` entry (C §9.1):

| Member | Type | Content |
|---|---|---|
| `role` | `token` | `checker`, `validator`, `interactor`; `generator`, `brute` |
| `profile` | `token?` | `testlib`, `simple`; `null` for `generator` and `brute` |
| `origin` | object | `{"kind":"built","language":ident,"entry":nsum(128)?}` or `{"kind":"prebuilt","program":nsum(128)}` (problem-relative path) |
| `build` | object? | the role build, in the `build` form above; `null` for a prebuilt or build-free role |
| `capture` | object | `{check: Check, sources: [nsum(128); 16], support_files: [nsum(128); 16], counts: {sources: u32, support_files: u32}, bytes: u64, lists_sha256: hex64, content_sha256: hex64}` as `capture` (§4.2), for the role's captured files (a prebuilt role: its program as the one source); `check` is the capture check of the role's source-consuming stage; `content_sha256` covers the copied bytes in the workspace design §3.2 file encoding |
| `provenance` | object | `{"kind":"built","header":Header}` or `{"kind":"declared"}` (prebuilt: custom exit remaps or the unexpected-EOF option cannot be detected, E §2.1) |

`Header` (candidate-only, E §2.1: never a claim about the header the
compiler used): `{"kind":"candidate","path":nsum(128),"sha256":hex64,
"pinned":bool}` (the single captured `testlib.h` beside every captured
source, the file a quoted include would resolve to first; `pinned` iff
its SHA-256 equals the tested header's), `{"kind":"not_identified",
"reason":token}` (profile `testlib`; reason `none`, `ambiguous`,
`not_beside_sources`, `angle_include`) or `{"kind":"not_applicable"}`
(profile `simple`; the roles `generator` and `brute`,
which have no profile). Human output says "header candidate", never "compiled
header". The compiler and flags are the role build's `executions[]`
entry (`program`, `spec`), named by `build.execution`.

### 4.5 Executions

`executions[]` entry, one per planned execution:

| Member | Type | Content |
|---|---|---|
| `index` | `u32` | plan index |
| `stage` | `token` | `build`, `run`, `command`, `hook`; `checker`, `validator`, `interactor`; `submit`, `submit_probe` (role `{"kind":"submit"}`, `{"kind":"submit_probe"}`; the artifact build keeps `{"kind":"build"}`) |
| `role` | object | `{"kind":"build"}`, `{"kind":"test","test":ident}`, `{"kind":"command","test":ident?,"iteration":u32}`, `{"kind":"raw"}`, `{"kind":"hook","name":ident,"trigger":token}`; `{"kind":"role_build","role":token}`, `{"kind":"checker","test":ident}`, `{"kind":"validator","test":ident}`, `{"kind":"interactor","test":ident}` (a role build has stage `build`; the solution run of an interactive test keeps `{"kind":"test"}`) |
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
| `class` | object? | the test's run class (`test` jobs); `null` without a record or for `cmd`; the test class of §2.15 with `io.output` (`streams` may then hold `output_file`, ≤ 3 entries) |
| `check` | object? | `null` for `cmd` and for an interactive test (§2.12); else §2.4, §2.11: `{"kind":"passed"}`, `{"kind":"failed","fail":"wrong_answer","reason":token,"diag":Diag}`, `{"kind":"unchecked","reason":"no_answer"}`, `{"kind":"checker_failed","class":"test_data_error","reason":"invalid_answer","diag":Diag}`, `{"kind":"check_error","os_error":i32?}`, `{"kind":"not_admitted","reason":token}` (`run_not_eligible`, `cancelled`), `{"kind":"internal_error","invariant":token}` (underivable, §2); `{"kind":"rejected","fail":token,"role":token}` (`wrong_answer`, `presentation_error`), `{"kind":"scored","raw":score,"scale":score,"role":token}`, `{"kind":"judge_failed","class":token,"role":token,"reason":token}`, and `unchecked` with the reasons `no_answer`, `checking_unverified`, `checking_interrupted`; `{"kind":"output_rejected","reason":token}` (`output_missing`, `output_misnamed`, `output_not_regular`, `output_unreadable`, `output_too_large`, `output_dir_too_large`, §2.15) |
| `diagnostic` | `bool` | the check was admitted after an unsuccessful class |
| `verdict` | object? | `null` for `cmd`; `{"kind":<§2.5 token>}`, `{"kind":"not_run","reason":token}`, `{"kind":"internal_error","invariant":token}` (invariant `internal_policy_error` for a session, §2.12); §2.5 tokens include `presentation_error`, `scored`, `session_limit`, `idle` |
| `score` | `score?` | §2.13; `null` without a points `scoring` or without a score |
| `validation` | object? | `null` without a validator; `{execution: u32, profile: token, outcome: token, stderr: nsum(1024)?}`, `outcome` `valid`, `invalid` (B6), `validation_failed` (B5), `validator_failed` (B7), `not_run` (B1–B4); `stderr`: the first 1 024 bytes of its stderr capture, `null` if none was stored |
| `judges` | `[Judge; ≤2]` | the test's checker (batch), or interactor then follow-up checker (interactive), in plan order; empty for kind `compare` |
| `session` | object? | `null` unless interactive; below |
| `output_file` | object? | `null` without `io.output` and for `cmd`; else the collection outcome of the solution run (E §5.6, §2.15): `{"kind":"collected","bytes":u64,"sha256":hex64,"path":rel}` (`path` `<run>/out/output`, ≤ 17), `{"kind":"missing"}`, `{"kind":"misnamed"}`, `{"kind":"too_many_entries"}`, `{"kind":"not_regular","type":token}` (`link`, `directory`, `other`), `{"kind":"unreadable","os_error":i32}`, `{"kind":"too_large","bytes":u64,"budget":u64}` (`bytes` the size observed, > `budget`; ≥ `budget` + 1 when it grew during the read), `{"kind":"not_collected","reason":token,"os_error":i32?}` (`io`, `timeout`, `unstable`; `os_error` `null` except for `io`), `{"kind":"not_attempted"}` |

`judge_failed`: `class` `declared_fail`,
`protocol_unsupported`, `report_invalid`, `artefact_invalid`, `crashed`,
`test_data_error` (checkers design §1); `role` `checker`, `validator`,
`interactor`; `reason` `declared_fail`, `exit_unsupported`,
`outcome_unsupported`, `points_not_enabled`, `outside_authority`,
`report_missing`, `report_not_regular`, `report_unreadable`,
`report_too_large`, `report_undecodable`, `report_malformed`,
`report_schema`, `points_invalid`, `disagreement`, `tout_missing`,
`tout_not_regular`, `tout_unreadable`, `tout_too_large`, `signal`, `exit_code`,
`wall_deadline`, `output_budget`, `dialogue_output`,
`peer_allowance_expired`, `invalid_input`, `validation_failed`.

`Judge`: `{role: token, execution: u32, profile: token,
result: JudgeResult?, message: nsum(4096)?, stderr: nsum(1024)?, report:
{bytes: u64, encoding: token?}?}`. `result` is the judge's own outcome as
its profile reads it (E §2.1): `{"kind":"passed"}`,
`{"kind":"rejected","fail":token}`, `{"kind":"scored","raw":score,
"scale":score}`, `{"kind":"judge_failed","class":token,"reason":token}`,
`{"kind":"unverified"}`, `{"kind":"not_read","reason":token}` (`stopped`,
`cleanup_incomplete`, `extraction_failed`); `null` when the run has no
record or did not start.
`report` (below) also names the extracted copy (E §2.1).
`message`: the report's message (`testlib`), its first 4 096 bytes as
UTF-8, `len` and `sha256` over the whole decoded message; `null` for
`simple` judges and when not read. `stderr`: the first 1 024 bytes of the
judge's stderr capture (the message of a `simple` checker). `report`:
`{path: rel? (≤ 20), bytes: u64, encoding: token?}`, the extracted copy
`<run>/judge/report`; `path` is `null` both for a report rejected before
extraction (missing, unsafe or oversized: `result` is then
`judge_failed{report_invalid}`, a judge failure) and for an extraction
failure (`result` `not_read{extraction_failed}` in batch and session
alike; in a batch test `tests[].check` is then B23's `check_error`,
infrastructure); the `result` tells them apart. Then the bytes read
(≤ 65 537) and the encoding (`windows_1251`, `utf_8`, `null` if
undecodable); `null` when not read.

`session` (checkers design §§5–6):

| Member | Type | Content |
|---|---|---|
| `policy` | `token` | `interactive_v7` |
| `checking`, `mode` | `token` | as `target.check.interactive` |
| `verdict` | object | `{"kind":"passed"}`, `{"kind":"rejected","fail":token,"role":token}`, `{"kind":"scored","raw":score,"scale":score,"role":token}`, `{"kind":"wall_deadline"}`, `{"kind":"output_budget"}`, `{"kind":"runtime_error"}`, `{"kind":"session_limit"}`, `{"kind":"idle"}`, `{"kind":"checker_failed","class":token,"role":token,"reason":token}`, `{"kind":"unchecked","reason":"checking_unverified"}`, `{"kind":"cancelled"}`, `{"kind":"infrastructure"}`, `{"kind":"internal_policy_error"}` |
| `decided_by` | object | `{"kind":"infrastructure","fact":u32}`, `{"kind":"decisive","fact":u32}`, `{"kind":"cancelled","fact":u32}` (the `CancelRequested`), `{"kind":"authority","fact":u32}` (`AuthorityComplete`), `{"kind":"invariant","rule":token}`, `{"kind":"transferred","role":token}` (cancellation snapshot, §2.12 I3); `fact` is the deciding fact's `obs_seq` |
| `facts` | `[Fact; ≤32]` | the closed record in `obs_seq` order (≤ 17 by construction), ≤ 16 384 bytes serialized |
| `deadlines` | object | `{solution_us: dur, interactor_us: dur, session_us: dur, peer_allowance_us: dur, idle_us: dur?}` |
| `directions` | `[Direction; ≤2]` | `s_to_i`, then `i_to_s` |
| `transcript` | object | `{"kind":"complete","dir":rel}`, `{"kind":"gapped","dir":rel,"gaps":u32,"dropped_records":u64,"dropped_bytes":u64}`, `{"kind":"failed","error":text,"at_offset":u64}`, `{"kind":"unavailable"}` (direct mode), `{"kind":"removed"}` (judge scratch, E §2.1); `dir` is `<run>/transcript` (≤ 19) |
| `seeds` | object | `{solution: Seed, interactor: Seed}`; `Seed`: `{"kind":"none"}` (not seed-controlled), `{"kind":"env","value":u64}`, `{"kind":"argv","value":u64}` |
| `early_reader_close`, `sigpipe_normalised` | `bool` | declared; whether checkers design §6.5 normalisation applied |
| `timing` | object | `{elapsed_us: dur, solution_cpu_us: dur?, interactor_cpu_us: dur?}`: the solution's deadline is not compensated for waiting on the interactor (checkers design §5.6) |
| `idle` | object? | `{limit_us: dur, elapsed_us: dur, read: [u64; ≤2], forwarded: [u64; ≤2], queued: [u64; ≤2]}` when the watchdog fired |
| `rerun` | object? | `{of_job: jid, of_test: ident, reproduced: bool}` for a seeded rerun (same seeds; `reproduced` iff the session verdict equals the original's; never a determinism, acceptance or timing claim, checkers design §7) |

- `Direction`: `{direction: token, read: u64?, forwarded: u64?,
  discarded_over_cap: u64?, delivery_incomplete: u64?, unread: token,
  queue_bytes: u64, queue_records: u32, end: token}`; the counters are
  `null` in direct mode (unavailable, never zero); `unread` is `none` or
  `unknown` (bytes never read from the pipe are not counted, checkers
  design §5.6); `end` is `eof`, `sink_gone` or `teardown`.
- `Fact`: a tagged object `{"kind":…,"obs_seq":u32,"decision_pos":u32,…}`
  with the variant's members after those two: `started` (`role`),
  `not_started` (`role`, `reason: {kind: token, code: i32, text:
  BText}`), `checker_skipped` (`reason`: `not_eligible`, `cancelled`),
  `stop_requested` (`role`, `cause`: `{"kind":"own_limit","limit":token}`
  with `wall` or `dialogue_output`, `{"kind":"peer_allowance_expired"}`,
  `{"kind":"session_limit"}`, `{"kind":"idle"}`,
  `{"kind":"teardown","reason":token}` with `peer_decided`, `cancel`,
  `infrastructure`), `cancel_requested`, `root_exited` (`role`, `status:
  Status` (§4.5), `provenance`: `{"kind":"natural"}` or
  `{"kind":"stopped","by":u32}`), `cleanup_result` (`role`, `cleanup:
  Cleanup` (§4.5)), `validation_done` (`role`, `outcome`:
  `{"kind":"ok","result":JudgeResult}`, `{"kind":"failed","class":token,
  "reason":token}`, `{"kind":"unverified"}`, `{"kind":"not_read"}`),
  `authority_complete` (`result: JudgeResult`), `infrastructure`
  (`detail: {kind: token, text: BText}`, `suppressed: u32`). Roles are
  `solution`, `interactor`, `checker`. `BText`: `{text: string, truncated:
  bool}`, the text ≤ 256 bytes after escaping, cut at an escape boundary.
  `Scored` values are canonical `score` strings.

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
internal: {invariant: token, locus: Locus, message: text}?, score:
Score?}`.

- `score` (§2.13): `null` without a points `scoring`;
  else `{"kind":"points","aggregation":token,"value":score,"max":score,
  "full":bool}`, `{"kind":"incomplete","aggregation":token,"missing":u32}`
  or `{"kind":"no_tests"}`.

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
  `infrastructure`, `not_run`, `internal_error`, and
  `presentation_error`, `scored`, `session_limit`, `idle`), `0` when
  absent, all zero without tests. `ByClass` has a `u32` member for each §2.2 class token and
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

- `fact`: `null` for jobs other than `test`; else `{outcome: FactOutcome,
  warnings: [token; ≤2]}` (`durability_uncertain`, `cleanup_pending`, also
  reported as `durability` warnings). `FactOutcome`: `{"kind":"published"}`,
  `{"kind":"superseded"}`, `{"kind":"not_published","reason":token,
  "message":text?}` (S §6.5: `sequence_unavailable`, `epoch_changed`,
  `fact_too_new`, `fact_unreadable`, `lock_busy`, `io`),
  `{"kind":"not_attempted","reason":token}` (`cancelled`,
  `not_completed`). `published` stays `published` whatever happens later.
- `hooks[]`: `{name: ident, trigger: token, execution: u32?, outcome:
  HookOutcome}`; `HookOutcome`: `{"kind":"succeeded"}`, `{"kind":"failed","class":
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
| `prune` | `job: jid?`, `detail: token` (`rename_failed`, `newer_schema`, `lease_unreadable`, `unmeasurable`, `measurement_incomplete`, `reclaim_leftover`, `reclaim_name_taken`; `test_dir_kept`, `test_dir_lost`) | S §6.3 at admission |
| `recovery` | `paths: u32`, `first: nsum(128)?` | S §4.5 |
| `persist` | `artefact: token` (`spec`, `record`, `cleanup_log`), `run: rid` (or an `irid` for an iteration run, §3.2; the grammars are disjoint, so the warning bound still holds at 756 B), `error: PersistError` | E §6.2, final at W |
| `durability` | `artefact: token` (`capture_manifest`, `job_metadata` (other job-directory metadata publications), `record`, `fact`, `stress_failure` for a promoted saved failure), `detail: token` (`uncertain`, `cleanup_pending`) | S §4.1 |
| `retention` | `detail: token` (`cwd_overflow`, `workspace_over_target`), `run: rid?` (or an `irid`), `bytes: u64` | S §6.4 |
| `cleanup_unverified` | `runs: u32` | E §4.4 |
| `fact` | `outcome` (§4.9 fact outcome) | S §6.5 |
| `hook` | `name: ident`, `outcome` (§4.9 hook outcome) | C §8.3 |
| `hooks_disabled` | `reason: token` | C §8.3 |
| `late_cancellation` | `phase: token` | §2.9 |
| `progress_dropped` | `lines: u32` | §8, final at E |
| `judge_scratch` | `run: rid` (or an `irid`), `detail: token` (`staged_copy_kept`, `transcript_kept`) | E §2.1 judge scratch |
| `testlib_header` | `role: token`, `detail: token` (`not_identified`, `not_pinned`, `declared`) | §4.4 `provenance` |
| `tool_unpinned` | none | submission contract §3.3: no version probe declared |
| `clipboard` | `outcome` (the `Submission.clipboard` object) | submission contract §9.3 |
| `submit_test_dir` | `detail: token` (`leftover`); the path is in `submission.external_dir` (within the 768-byte warning bound: 358 B) | submission contract §7.3 |
| `io_scratch` | `run: rid`, `detail: token` (`staged_input_kept`, `output_kept`, `partial_copy_kept`, `not_measured`: measurement and clearing of the run's `cwd/` were skipped because the output-file worker was transferred or `cwd/` no longer names H) | E §5.6 I/O scratch |

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

### 4.14 `stress`

Present iff `command` is `stress` (stress contract). Every count is
cumulative over the job.

| Member | Type | Content |
|---|---|---|
| `mode` | `token` | `search`, `replay`, `replay_binaries` |
| `shape` | `token` | `batch`, `interactive` |
| `seeds` | object | `{start: u64, last: u64?}`: the start seed (§2.1 of the stress contract; drawn or given) and the generator seed of the last admitted iteration (`null` when none) |
| `budget` | object? | `{iterations: u64, time_us: dur}` as effective (CLI over `[stress]`); `null` for a replay job (stress contract §8) |
| `stop` | object | `{reason: token, iteration: u64?}`: stress contract §5 stop reasons `cancelled`, `build_failed`, `failure`, `infrastructure`, `admission_closed`, `cleanup_failed`, `iterations`, `time_budget`, `counter_limit`, `replay`; `iteration` the k whose admission check decided it (`null` for `build_failed`) |
| `iterations` | object | `{admitted: u64, retired: u64, saved: u32, promoted: u32, not_saved: u64}`; `saved` counts failures staged with disposition `save`, `promoted` those published, `not_saved` failing iterations counted only (cap) or whose staging failed |
| `runs` | `u64` | iteration runs started (coverage `started`) |
| `captured_bytes` | `u64` | stored stdout and stderr bytes over every iteration run |
| `binaries_bytes` | `u64` | preserved-program bytes copied into saved failures (≤ 256 MiB) |
| `classes` | `[ClassAggregate; ≤8]` | one per class that occurred, in the order `passed`, `generator_failure`, `validator_failure`, `oracle_failure`, `judge_failure`, `candidate_failure`, `infrastructure`, `cancelled` |
| `failures` | `[FailureRef; 20]` | the first 20 iterations of a failing class, in iteration order, then `failures_dropped: u64` (the one capped list whose dropped count is a `u64`: failing iterations are unbounded) |
| `timing` | `[RoleTiming; ≤8]` | one per run role that started, in stress contract §3.1 order |
| `replay` | object? | `{of: fid, original_class: token, reproduced: bool, from_input: bool, binaries: bool}` for a replay job (stress contract §8), else `null` |
| `cleanup_failed` | object? | `{paths: [nsum(256); 16], paths_dropped: u32}`: job-relative leftover paths (stress contract §6.4), else `null` |

- `ClassAggregate`: `{class: token, count: u64, first: {iteration: u64,
  seed: u64}, diagnostics: [StressDiag; 16], diagnostics_dropped: u32}`;
  `first` is the first iteration of the class; `passed` has no
  diagnostics.
- `StressDiag`: `{iteration: u64, seed: u64, role: token, reason: token,
  source: token, text: nsum(1024)}`; `source` `message` (a judge's
  message), `stderr` (the deciding run's stderr head), `compare_diag` (the
  comparator's `Diag` rendered as in human output) or `none` (empty
  `text`).
- `FailureRef`: `{id: fid?, iteration: u64, seed: u64, class: token, role:
  token, reason: token, saved: Saved, runs: [FailureRun; ≤8]}`; `id` is
  `null` for a counted-only iteration. `Saved`:
  `{"kind":"promoted","path":rel,"binaries":token}` (`path`
  problem-workspace-relative `problems/<p>/stress/failures/<id>` ≤ 166;
  `binaries` `all`, `partial`, `none`),
  `{"kind":"not_promoted","reason":token,"path":rel,"message":text?}`
  (`lock_busy`, `exists`, `cross_filesystem`, `unsupported`,
  `problem_changed`, `path_blocked`, `io`; `path` job-relative
  `failures/<id>` ≤ 85, where the staged directory stays),
  `{"kind":"not_saved","reason":token}` (`cap`, `retention_budget`, `io`,
  `manifest_too_large`, `resources_held`, `replay`).
- `FailureRun`: `{role: token, class: object?, record: Ref?}`: the run's
  §2.2 class (`null` without a record) and its persisted `record.json`
  (`iterations/<irid>/record.json`, ≤ 64) for a saved or kept iteration,
  `null` for a counted-only one; a `Ref` error is the iteration
  artefact's `PersistError` (E §6.2).
- `RoleTiming`: `{role: token, runs: u64, wall_us: {min: dur, max: dur,
  total: dur}?}` over the role's started runs with a measured wall (E
  §6.3); `null` when none. `total` uses checked addition (a sum of
  measured walls cannot exceed `u64` microseconds in a job's lifetime).
  Per-iteration timing is not reported.

### 4.15 `stress_failures` document

`cpf stress ls --json` (stress contract §2.3): `{"schema":1,"document":
"stress_failures","platform":…,"problem":pident,"entries":[Entry; 10000],
"entries_dropped":u32}`, entries in byte order of the entry name, not
persisted. `Entry`: `{name: nsum(256), state: token, detail: text?, id:
fid?, class: token?, role: token?, reason: token?, iteration: u64?, seed:
u64?, shape: token?, created_at: time?, binaries: token?}`; `state` is the
stress contract §7.6 classification (`valid`, `unexpected`, `missing`,
`unreadable`, `malformed`, `too_new`), `detail` the reason for any other
state, and the members after `detail` are non-null iff `valid`;
`binaries` is `all`, `partial` or `none` (from the manifest's `[[build]]`
entries). A selection or workspace failure writes the §4.12 `error`
document (`command: "stress"`).

## 5. Exit statuses

### 5.1 Codes (decision D1)

| Code | Name | Meaning |
|---|---|---|
| 0 | `success` | the command did what was asked; jobs: `passed`, `succeeded`, `scored` with a full aggregate |
| 1 | `failed` | the solution or a program failed: `wrong_answer`, `wall_deadline`, `output_budget`, `signal`, `nonzero_exit`; `presentation_error`, `session_limit`, `idle`, `scored` with an aggregate that is not full; `candidate_failure`; `interactor-test`: a `good` solution failed or a `bad` solution passed (§5.2) |
| 2 | `usage` | invalid arguments (clap) or a rejected combination: `--json`/`--events` with raw `cpf run`, `--json` with `--events`, `--problem` with `switch`, both test sources on stdin; an organisation argument outside its grammar (tag, status name, filter value, search term), `--problem` with `tag`/`untag`/`mark`, `mark` or `notes` on `contest:<name>` (S §5.9); `--problem` with `fetch`, `archive`, `restore` or `quarantine`; for `fetch`, `--only` naming a label outside the enumeration or selecting more than 50 members (S §5.8.1, refused after the one enumeration request and before any member request), `--only` or `--all-in-section` with a selection they do not apply to, `--history` with `--json`; for `archive`, `--plan` without `--apply`, `--resume` or `--close` with a target or a plan option; `restore --into` with a contest snapshot (archive contract §7.4); `--json` or `--events` with `interactor-test` |
| 3 | `build_failed` | the build failed; the submission tool failed (`submit_failed`) |
| 4 | `not_verified` | nothing failed but not everything was checked: `unchecked` (no answer; a judge's cleanup not verified), `no_tests`; `no_iterations`; `interactor-test`: an expectation not decided (§5.2) |
| 5 | `checker_failed` | judging could not decide: the comparator's invalid answer; a checker, validator or interactor failure of any class (checkers design §1), including invalid test data; `judge_failure`, `oracle_failure`, `validator_failure`, `generator_failure` (a stress harness role failed; no solution verdict) |
| 6 | `cancelled` | cancellation (API, Windows, or Unix when not re-raised, §5.3) |
| 7 | `config` | failure class `config`: C §3 phases 1–3, unsupported problem kind (C §9), missing context (C §8.1), accident guard (S §7); a workflow status outside the vocabulary, or an undeterminable `status.values` (S §5.9.4, C §7.5); the verdict `submit_version_mismatch`, an empty `solution_files` for `submit-prep` |
| 8 | `workspace` | failure class `workspace`: no workspace, selection errors (S §5.7, `NoSelection`), record classification failures, unknown or unhealthy test, lock `Busy`, admission refused, freeze, capture or manifest failures, storage I/O; a submission version not published; a fetch refused at S §5.8.4 steps 0 or 2 (including `record_too_large`, a journal that remains, `history_blocked`, `fetch_inconsistent`) or failed in storage (S §5.8.8 outcome `failed` without a sites §8.3 class), a refetch of a problem or contest without a remote identity and URL |
| 9 | `partial` | failure class `partial`: a metadata command committed part of its work, or kept the existing state where the update it was asked for conflicted with it; what it committed stays committed, and a rerun or a user action continues it (S §5.2 step 5, S §5.3 step 4, `clean --apply`); for `fetch` (S §5.8): a **kept conflict** (outcome `conflict`: S §5.8.5 ambiguity, `base_changed`, `edited_during_commit`; or the finding `metadata_conflict{field}` on a committed problem; an edited sample kept while upstream changed is only the warning `edited_sample_kept`, not a kept conflict), a committed problem with `metadata_not_applied`, `metadata_pending`, `history_pending`, `history_blocked` or `journal_pending` (S §5.8.8), a contest or section selection in which some selected members ended `committed` or `unchanged` and others did not, the step-8 membership edit not committed after a member committed (S §5.8.13); a snapshot was committed but retirement did not happen or cleanup ended `incomplete`, a restore published the problem but not its history, a quarantine purge left a selected entry (archive contract §5.4, §6.6, §7.3); a submission version published without its `current` reference |
| 10 | `infrastructure` | cpf could not run or observe a program reliably; `stress_cleanup_failed` (an iteration's files or resources could not be released) |
| 11 | `delivery` | requested machine output could not be written (§6.3, §7.6) |
| 12 | `site` | failure class `site`: `cpf fetch` failed with a sites contract §8.3 class (`auth_required`, `challenge`, `rate_limited`, `not_found`, `not_available`, `unsupported`, `unrecognized`, `parser_failure`, `network`, `offline_miss`) for its URL, its enumeration, or a problem, and no earlier row of §5.2 applies |
| 70 | `internal` | failure class `internal`: a bug detected and handled (§5.4) |

13–63 are reserved for later features; of 64–78 (`sysexits`) only 70 is
used; 126, 127 and ≥ 128 are never chosen as exit codes (the Unix re-raise
of §5.3 is a death by signal).

### 5.2 Precedence

For every command, job or not, the status is decided by the **first** true
condition:

| # | Condition | Status |
|---|---|---|
| 1 | failure class `usage` (nothing started; for a `fetch` bulk refusal, nothing sent or written after the one enumeration request) | 2 |
| 2 | **forced** cancellation (E §8.2 force exit; delivery abandoned) | §5.3 |
| 3 | failure class `internal` (any time, including after F), or job verdict `internal_error` | 70 |
| 4 | machine output requested and its delivery failed (decision D9) | 11 |
| 5 | orderly cancellation: `result.cancellation` set at F, failure class `cancelled`, job verdict `cancelled`, or cancellation of a `hook` job (§2.10) | §5.3 |
| 6 | failure class `config`; job verdict `submit_version_mismatch` | 7 |
| 7 | failure class `workspace`; submission `publication.kind = "not_published"` | 8 |
| 8 | failure class `partial`; submission `published` with `current = false` | 9 |
| 9 | failure class `site` | 12 |
| 10 | job verdict `infrastructure`; `stress_cleanup_failed` | 10 |
| 11 | job verdict `build_failed`; `submit_failed` | 3 |
| 12 | job verdict `checker_failed`; `judge_failure`, `oracle_failure`, `validator_failure`, `generator_failure` | 5 |
| 13 | job verdict `wall_deadline`, `output_budget`, `signal`, `nonzero_exit`, `wrong_answer`; `session_limit`, `idle`, `presentation_error`; `candidate_failure` | 1 |
| 14 | job verdict `unchecked`, `no_tests`; `no_iterations` | 4 |
| 15 | job verdict `scored` and `result.score.full = false` | 1 |
| 16 | otherwise (including `scored` with a full aggregate) | 0 |

- Rows 6–8 apply to job refusals and to non-job command failures alike
  (`cpf current` with `NoSelection` → class `workspace` → 8; `cpf config
  show` on invalid configuration → class `config` → 7). Row 9 applies to non-job commands only.
- Pairs: forced cancellation + anything → row 2; internal + delivery
  failure → 70; delivery failure + orderly cancellation → 11; delivery
  failure + any verdict other than `internal_error` → 11; refusal +
  delivery failure → 11; `infrastructure` + `wrong_answer` → 10 (via the
  verdict).
- `pre_delivery_exit` is rows 3 and 5–16 evaluated at F (row 9 never
  holds for a job).
- A display profile (§2.8) never changes the status:
  `presentation_error` exits 1 under `judge-cf` too. An `incomplete`
  score never reaches row 15 (§2.13). `internal_policy_error` is an
  internal failure (row 3, 70).
- For `cpf switch`, the `hook` job selects a row only through row 3
  (verdict `internal_error` or a caught panic, class `internal`) or row 5
  (cancellation); its ordinary verdicts and its admission failures of class
  `workspace` or `config` select none (§2.10). Warnings, persistence
  failures, fact outcomes, **ordinary** `after_test` hook outcomes and
  stderr write failures never select a row; an `internal_error` hook
  outcome selects row 3.
- **Several problems or jobs.** A command that acts
  on several problems (`cpf fetch` of a contest or section, `cpf archive
  contest:<c>`, a contest `cpf restore`) takes the first row that any of
  its conditions satisfies, over the selection and every problem: a bulk
  fetch with one member refused exits 8, with a kept conflict or with
  some members failing while others committed 9, and 12 only when no
  member committed or was unchanged. `cpf interactor-test` evaluates rows
  1–12 over the conditions of all its jobs (it has no machine output, so
  row 4 never holds) and stops admitting jobs after the first job whose
  own status comes from rows 2–12; rows 13–16 are replaced by its
  expectations, the predicates of C §9.1.7 evaluated per job: a `good`
  job meets its expectation iff its job verdict is `passed` or `scored`
  with a full aggregate; a `bad` job meets it iff at least one of its
  tests has a solution verdict (as listed there) or a non-full score,
  whatever its job verdict. Then: a `good` job whose own status would be 1
  (a solution verdict, or a non-full aggregate), or a `bad` job whose
  verdict is `passed` or `scored` with a full aggregate, gives 1 (an
  expectation contradicted); otherwise any job that does not meet its
  expectation gives 4 (not decided: a `good` job whose own status would
  be 4, a `bad` job with neither such a test nor a pass); otherwise 0.
- Class `site` arises only in `cpf fetch`, which has
  no job; it is placed after `partial` so that a partly committed bulk
  fetch reports what it committed (9), and before every verdict row.
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
| `cpf test add` | 0, 8, 9; also with `--from-stress` |
| `cpf stress` (search, `--replay`, `--replay-binaries`) | 0, 1, 3, 4, 5, 7, 8, 10, 11, cancellation |
| `cpf stress ls` | 0 (any entries, any classification), 8, 11 (`--json`) |
| `cpf test register`, `enable`, `disable` | 0, 8 |
| `cpf test ls` | 0; 8 when the catalog is not `missing` or `valid` (listing still printed) |
| `cpf config show` | 0, 7, 11 (`--json`, C §10) |
| `cpf config check` | 0, 7 (no `--json` in schema 1) |
| `cpf config paths` | 0 |
| `cpf doctor` | 0, 7; an unsupported capability is never an error |
| `cpf status` | 0, 8, 11 (`--json`); filters add only 2 for a filter value outside its grammar |
| `cpf clean` | 0, 8; `--apply`: 9 when a selected candidate could not be renamed or deleted (held leases are skipped) |
| `cpf submit-prep`, source copy | 0, 7, 8, 9, 11 (`--json`) |
| `cpf submit-prep`, stage mode | 0, 1, 3, 4, 5, 7, 8, 9, 10, 11, cancellation |
| `cpf fsck` | 0, 8 (findings are reported with 0) |
| `cpf adopt`, `ignore`, `gitignore` | 0, 8 |
| `cpf tag`, `cpf untag` | 0, 8 |
| `cpf mark` | 0, 7, 8 |
| `cpf notes` | 0, 8, 11 (`--json`); `--path`: 0, 8, 11 |
| `cpf ls` | 0 (also with invalid entries and findings), 8, 11 (`--json`) |
| `cpf search` | 0 (hits or none, complete or incomplete), 8, 11 (`--json`), cancellation |
| `cpf archive` | plan: 0, 7, 8, 11 (`--json`); `--apply`: 0, 7, 8, 9, 11; `--resume`: 0, 8, 9, 11 (`--json`); `--close`: 0, 8, 9 (archive contract §5.4) |
| `cpf restore` | 0, 8, 9, 11 (`--json`); 7 when a snapshot id is looked up and the configuration that holds `archive.roots` is invalid (a snapshot path loads no configuration) |
| `cpf quarantine ls` | 0, 8 |
| `cpf quarantine purge` | 0, 8; `--apply`: 9 when a selected entry could not be renamed or deleted |
| `cpf ls --archived` | as `cpf ls`, plus cancellation |
| `cpf fetch` | 0, 7, 8, 9, 11 (`--json`), 12, cancellation |
| `cpf fetch --history` | 0, 8 |
| `cpf interactor-test` | 0, 1, 3, 4, 5, 7, 8, 10, cancellation |
| `cpf --help`, `--version` | 0 |

"Cancellation" is the §5.3 status. Raw `cpf run` never passes the child's
code through; the child's status is in the report and the stderr summary
(D2). `cpf check` (design §5.4) gets its row with its feature.

## 6. `--json`

### 6.1 Availability

- Accepted by the job commands `cpf build`, `cpf test`, `cpf cmd`,
  `cpf stress` (every mode) and `cpf submit-prep` in stage mode (the
  `job_report`), each with `--events` too, and by non-job commands whose
  contract defines a document (schema 1: `cpf config show`, `cpf status`
  below, `cpf ls`, `cpf search`, `cpf notes` below, `cpf stress ls`
  (§4.15), `cpf submit-prep` in source-copy mode (the `submit` document
  below, no `--events`), `cpf archive` (plan, `--apply` and `--resume`),
  `cpf restore`, `cpf fetch` (the `fetch` document below, written once a
  problem transaction has begun; a failure before the first one, i.e.
  while the S §5.8.1 selection is resolved (an unsupported URL, a failed
  enumeration, the bulk refusals), writes the §4.12 `error` document with
  `command` `fetch` and its class, `site` or `usage` among them); later
  `cpf test ls`). A per-command flag; elsewhere clap rejects it (2):
  `cpf tag`, `untag` and `mark` have no document; nor have `cpf archive
  --close` and `cpf quarantine`; nor have `cpf fetch --history` and `cpf
  interactor-test` (neither accepts `--events` either: its jobs' reports
  are in their job directories).
- Raw `cpf run` rejects `--json` and `--events` (E §8.1): 2.
- `--json` with `--events`: 2 (decision D4).

**`status` document** (workspace design §3.2, §3.3), schema 1,
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
| `judge` | token | the judge fingerprint (C §9.1.3) of the fact against the current one: `none` (neither has one), `unchanged`, `changed` (they differ, including one of them `null` or absent), `unknown` (fact not `valid`, or the current value not computable) |

Human `status` says "unchanged" only when `comparison`
is `unchanged` and `judge` is `unchanged` or `none`; a checker,
validator, interactor or `[check]` edit is reported as "judge changed".

Fixture: `src/report/wire/fixtures/status.json`.

The status `Problem` gains, after `findings` (below), the member
`submit`: `{state: token, id: string?, mode: token?, tested: bool?}`,
`state` `none`, `current`, `stale`, `unknown` (submission contract §10;
`id`, `mode`, `tested` from the manifest, `null` for `none` and when the
manifest is not `valid`). `cpf submit-prep` accepts `--json` (source copy:
the `submit` document `{"schema":1,"document":"submit","platform":…,
"problem":pident,"submission":Submission,"warnings":[Warning; 24]}`, not
persisted; stage mode: the `job_report`) and `--events` in stage mode only.
Fixtures `submit.json`, `submit_stage.json` (submission contract SB-R1).

**Fetch state in `status`.** The status `Problem` gains, after `submit`,
its last member `fetch`: `FetchState?`, `null` for a local problem, else
`{record: token, attempted_at: time?, outcome: token?, txn: jid?,
recovered: bool?, incomplete: token?}`. `record` is the S §9 record
classification of `.cpforge/fetch/<p>/status.toml` (`missing`,
`unreadable`, `malformed`, `too_new`, `valid`); `attempted_at`, `outcome`
(`committed`, `unchanged`, `conflict`, `refused`, `failed`,
`rolled_back`), `txn` and `recovered` are that record's fields (S §5.8.9),
non-null iff `record` is `valid` (`txn` also `null` when the record has
none); `incomplete` is the `fetch_incomplete` phase of S §5.8.12
(`pending`, `committed`, `finished`, `undetermined`) while a journal of
the problem exists, else `null`. Superseded and unregistered upstream
files stay in `diagnostics`. Fixtures `status_fetch.json` (a committed
fetch) and `status_fetch_incomplete.json` (a journal pending).

**Organisation fields in `status`.** `tags` and
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

**Common types.** `OrgFinding`: `{field: token,
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

**`ls` document**: `{"schema":1,"document":"ls",
"platform":…,"kind":token,"problems":[LsProblem],"contests":[LsContest],
"invalid":[Source]}`. `kind` is `problems` (`contests` empty) or
`contests` (`problems` empty), in S §5.9.7 order.

| `LsProblem` member | Type | Meaning |
|---|---|---|
| `problem` | `pident` | identity (local or remote, §3.2) |
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

**`search` document**: `{"schema":1,"document":
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

**`notes` document**: `{"schema":1,"document":
"notes","platform":…,"problem":pident,"path":ns,"state":token}`; `path` is
the absolute native path of `notes.md` (Windows without the verbatim
prefix, S §5.7); `state` is `created` or `existing` (mutating form), or
`absent`, `existing`, `not_regular` (`--path`).

Fixtures: `status_invalid_fields.json` (above), `src/report/wire/fixtures/ls.json`, `ls_contests.json`,
`search.json`, `search_incomplete.json`, `notes.json` (encoder vectors and
the Python oracle round trip, as for `status`).

**Snapshot results.** `origin` gains its value
`snapshot`. `LsProblem` and `Hit` gain a last member `snapshot:
Snapshot?` (`null` for `active`): `Snapshot` = `{id: string, kind: token
(problem | contest), root: ns, path: string, complete: bool}`, `root` the
canonical root, `path` root-relative with `/`. For a snapshot, the
`LsProblem` fields come from its summary (archive contract §4.5) and
`contests` lists the summary's contest directories. The `ls` document's
`kind` gains `archived` and `archived_contests`; `LsContest` gains the
last member `snapshot: Snapshot?`, `null` for an active contest (for an
archived contest `members` is the planned count and `counts` empty).
`Source.kind` gains `root_unavailable`, `not_an_archive_root`,
`root_record_unreadable`, `root_record_malformed`, `root_record_too_new`,
`roots_unavailable`, `unreadable_directory`, `summary_malformed`,
`summary_too_new`, `summary_too_large`, `manifest_missing`; `path` is then
the absolute native path. The `search` document gains `archives: bool`
(false with `--no-archives`) after `notes`.

**`archive_plan` document** (plan mode, `--json`):
`{"schema":1,"document":"archive_plan","platform":…,"plan":hex64,"target":
ArchiveTarget,"problems":[PlanProblem],"warnings":[token]}`. `ArchiveTarget`: `{kind:
token (problem | contest), identity: string, dir: ident}`. `PlanProblem`:
`{problem: pident, dir: ident, decision: token (retire | export |
snapshot_only), reason: token?, destination: {root: ns, path: string},
cleanup: token, classes: [{class: token, files: u32, bytes: u64, kept:
bool}], other_files: [string], omissions: [{what: token, count: u32,
bytes: u64}], externals: [{kind: token, value: nsum(512), key: string}],
selection: bool, contests: [ident], delete_warning: text?}`. `warnings`:
`network_root`, `root_not_in_roots`. Lists are bounded by the archive
contract's limits, not capped. The binding whose hash is `plan` (archive
contract §3.7) is the canonical JSON of `{cpf_version, target, problems:
[{problem, dir, source_object, entries: [{tree, path, kind, size, sha256,
exec, class, decision}], config_sha256, references}], archive_config,
destination}`, written only to compute the hash.

**`archive` document** (`--apply`, `--resume`): `{"schema":1,
"document":"archive","platform":…,"plan":hex64?,"problems":[ArchiveOutcome],
"contest":ContestOutcome?,"resumed":[ArchiveOutcome]}`. `ArchiveOutcome`: `{problem:
pident, snapshot: Snapshot?, reused: bool, retirement: token (retired |
exported | snapshot_only | refused), reason: token?, txn: jid?,
selection_cleared: bool, cleanup: ArchiveCleanup?}`. `ArchiveCleanup`: `{state: token
(complete | incomplete), trashed: u32, quarantined: u32, deleted: u32,
snapshot_only: u32, quarantine: jid?, unsettled: [{path: string, reason:
token}; 64], unsettled_dropped: u32}`. `ContestOutcome`: `{contest: ident,
snapshot: Snapshot, complete: bool, retired: bool}`. A refusal before any
snapshot writes the §4.12 `error` document with `command` `archive`.

**`restore` document**: `{"schema":1,"document":
"restore","platform":…,"snapshot":Snapshot,"restored":[{problem: pident,
dir: ident, origin: string?, history: token (restored | not_restored |
pending)}],"contest":ident?,"omissions":[{what: token, count: u32, bytes:
u64}],"externals":[{kind: token,
value: nsum(512), key: string, exists: bool?}],"findings":[token]}`;
`exists` is `null` for programs and for truncated values (archive contract
§7.3). A refusal writes the `error` document with `command`
`restore`.

Fixtures: `ls_archived.json`, `search_archives.json`, `archive_plan.json`,
`archive.json`, `archive_incomplete.json`, `restore.json` (encoder vectors
and the oracle round trip).

**`fetch` document** (`cpf fetch --json`; sites contract §11.2, storage
contract §5.8): `{"schema":1,"document":"fetch","platform":…,"selection":
FetchSelection,"problems":[FetchProblem],"network":FetchNetwork,
"cancelled":bool}`, written once a problem transaction has begun; not
persisted. `problems` holds one entry per selected member whose attempt
began, in selection order (≤ 50, S §5.8.1), or the one problem.

- **Cancellation.** The document is written also when cancellation intent
  ends the command (then the §5.3 status follows, §5.2 row 5; a delivery
  failure gives 11, row 4). `cancelled` is true iff intent was set before
  the command finished. A member whose attempt was cut by cancellation
  before its commit has `outcome` `cancelled` (nothing committed; what its
  attempt left is S §5.8.8's); a member already committing finishes and
  reports its own outcome (`../commands.md`, "Interrupting"); members not
  started are omitted, so `problems` may hold fewer entries than
  `selection.members`. Intent before the first problem transaction began
  writes the §4.12 `error` document (class `cancelled`) instead.

- `FetchSelection`: `{kind: token, identity: string?, url: string,
  members: u32}`; `kind` `problem`, `contest`, `problem_set` (the S §5.8.1
  selection variants); `identity` the problem's or contest's identity,
  `null` for a `problem_set`; `url` the adapter's canonical URL; `members`
  the selected member count (1 for `problem`).
- `FetchProblem`: `{identity: pident, outcome: token, failure:
  FetchFailure?, provenance: Provenance?, warnings: [FetchNote], findings:
  [FetchNote], txn: jid?}`; `outcome` the S §5.8.9 status outcome
  (`committed`, `unchanged`, `conflict`, `refused`, `failed`,
  `rolled_back`), or `cancelled` (above, document only); `txn` the
  transaction id when a journal was written.
- `FetchFailure` (`null` unless the problem failed with a sites §8.3
  class): `{class: token, reason: token?, http_status: u32?, retry_at:
  time?}`; `class` one of `auth_required`, `challenge`, `rate_limited`,
  `not_found`, `not_available`, `unsupported`, `unrecognized`,
  `parser_failure`, `network`, `offline_miss`; `reason` the class's reason
  where it carries one (`network{…}`, `unrecognized{redirect}`);
  `retry_at` when present (`rate_limited`).
- `Provenance` (sites §8.1; `null` when no response was obtained): `{url:
  string, final_url: string, fetched_at: time, status: u32, body_sha256:
  hex64, from_cache: bool}`.
- `FetchNote`: `{kind: token, ordinal: u32?, test: ident?, detail:
  string?}`, the S §5.8.9 finding record (`kind`, optional `ordinal`,
  optional `test`) with `detail` the payload a finding or warning carries
  in braces (`metadata_conflict{field}`, `metadata_not_applied{reason}`,
  `history_blocked{path, classification}`, `edited_sample_kept{test,
  upstream}`, `limits_unparsed{field}`), `null` otherwise. `warnings`
  holds the sites §8.2 hints and warnings (`no_samples`,
  `tolerance_hint`, `multiple_answers_hint`, `language_fallback`,
  `limits_unparsed`, `title_unparsed`, `interactive_samples_skipped`,
  sites §8.4) and the storage warnings of the attempt (`zero_samples`,
  `edited_sample_kept`, S §5.8.5; `status_not_written`, S §5.8.9;
  `durability_uncertain`, `cleanup_pending`, S §5.8.8); `findings` the
  storage findings of S §5.8.4–§5.8.8, in the order they were recorded:
  the ambiguity findings `duplicate_ordinal`, `duplicate_recorded`,
  `duplicate_upstream`, `moved`; the pair outcomes `added`, `updated`,
  `kept_edited`, `removed_upstream`, `removed_locally` (with `ordinal`,
  and `test` where an entry exists); `name_taken`, `base_changed`,
  `edited_during_commit`, `metadata_conflict`, `metadata_not_applied`,
  `metadata_pending`, `history_pending`, `history_blocked`,
  `journal_pending`, `abort_unrecorded`.
- `FetchNetwork`: `{wire_requests: u32, connection_attempts: u32,
  redirects: u32, retries: u32, revalidated: u32, offline: bool,
  elapsed_ms: u64, budget_requests: u32, budget_seconds: u32}`;
  `budget_requests` is `200` and `budget_seconds` `900` (sites §3.4).

Fixtures: `fetch.json`, `fetch_contest_partial.json`, `fetch_cancelled.json`
(a contest of three members, intent set during the second member's
network phase: the first `committed`, the second `cancelled`, the third
omitted, `selection.members` 3, `cancelled` true, cancellation status)
(encoder vectors and the oracle round trip).

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
| `output_chunk` | `run: rid, stream: token, bytes: bytes` (≤ 4096 raw bytes; `=output` only); `run` is an `irid` for an iteration run (5 656 B at most, within the event-line bound) |
| `run_finished` | `run: rid, summary: RunSummary` |
| `progress` | `done: u32, total: u32` |
| `job_finished` | `job: jid, verdict: token, report: {path: rel}?, dropped: u64` |
| `iteration_started` | `iteration: u64, seed: u64, seeds: {interactor: u64?, solution: u64?}` |
| `iteration_run_started` | `run: irid, stage: token, role: token, digest: hex64` (stages `generator`, `brute`, `run`, judges' stages; `role` the run role) |
| `iteration_run_finished` | `run: irid, summary: RunSummary` (≤ 6 898 B, within the 7 KiB event-line bound) |
| `iteration_finished` | `iteration: u64, class: token, role: token, reason: token, saved: fid?` |

In a `stress` job, builds emit `run_started` and
`run_finished` with their run ids; iteration runs emit only the iteration
events, which carry no `rid`; no `progress` event is emitted after the
builds. Every iteration event is non-terminal and droppable (§7.4); `seq`
is a `u64`, so streams longer than 10 000 executions keep the gap
equation.

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
  - **Stress result lines** (stress contract §9), in
    the final batch before the summary: one budget line ("<retired>
    iterations from seed <start> in <elapsed>"), one stop line (reason and
    iteration), one line per class that occurred (≤ 8, with count and the
    first iteration and seed), one line per failure ref (≤ 20: id or
    "not saved", class, deciding role, reason, seed, saved outcome), one
    line per failing class with its first diagnostic (≤ 5), one line per
    run role timing (≤ 8), one saved-failure line (saved, promoted, not
    saved, binaries), one replay line (replay jobs: "reproduced" / "not
    reproduced" and both classes), one cleanup line (≤ 1): at most 46
    lines, within the 128 stress aggregate lines of the final allowance.
    Every string in them is sanitized and cut as every line.
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
  filled (64 diagnostics, 29 warning lines, 16 summary lines; the stress
  aggregate lines are R-S07, §9.7), sink double blocked until after D → peak queued
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
  (Stress: the stalled-then-resumed stress case with its final aggregates
  is R-S07, §9.7.)
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
  stated bounds; 10 000 planned executions of kind
  `compare` within B.
- Worst-case vectors: a `judges[]` entry with a 4 096-byte
  message of `"` and `\` characters, a message of control characters (base64
  form) and a 4 097-byte message (truncated, digest of all bytes), each with
  a 1 024-byte stderr diagnostic at its maximum; a `session` with 17 facts
  at their maximum (both text fields at 256 escaped bytes, truncated at an
  escape boundary) and every optional member set; a `roles[]` entry with 16
  + 16 maximal paths, a failed build with 16 names and a captured header; a
  `score` of 14 integer and 9 fractional digits; each within its row. For
  every shape of the T formula, a full plan (the most tests the caps allow)
  serialised with every entry at its worst stays within B; the report
  reservation equals 2 × B; a plan with 2 × B > 256 MiB is refused before
  admission. Near-cap vectors: 1 000 interactive tests with a validator
  and a follow-up checker plus 5 378 hooks (2 × B = 268 423 168 B,
  admitted) and plus 5 379 hooks (refused); kind `compare` at 10 000
  executions (251 142 144 B, admitted).
- Document-kind vectors (§3.7.2): a header of each job kind at its worst
  (a `test` job with every role, `target.check`, `target.io` and both
  `null` members at their maxima ≤ 128 KiB; a `submit` job with a
  worst-case `Submission`, submission contract SB-R2, ≤ 145 KiB, and its
  reservation 2 × B with H_submit; a `stress` job with 24 `persist`
  warnings carrying maximal `irid`s ≤ 160 KiB); an `error` document with 64
  maximal diagnostics ≤ 80 KiB; a `submit` document with 24 maximal
  warnings ≤ 35 KiB; a `stress_failures` document with 10 000 maximal
  entries ≤ 14 MiB.
- `score` encoding: 0 → `"0"`; 10⁻⁹ → `"0.000000001"`;
  12.5 → `"12.5"`; 10¹³ → `"10000000000000"`; readers reject `"012"`,
  `"1.50"`, `"1."`, `".5"`, `"-1"`, `"1e3"` and a JSON number.
- A `report.json` with `schema` 2 is never pruned, identified from its
  prefix.

### 9.5 Display

- The label of every verdict and build result is exactly its §2.8 entry;
  each job summary equals its template; `passed` with cleanup `Unverified`
  → "OK (cleanup not verified)".
- `presentation_error` → `PE` under display `cpf` and
  `WA` "presentation error" under `judge-cf`, with byte-identical documents
  apart from `target.check.display` and status 1 in both; the checker,
  interactive and points summary templates; `CHECKER-FAILED` qualifiers per
  class and role; `SCORE` with a full and an incomplete aggregate; `CE`
  "checker build"; a judge message containing "accepted" is shown quoted
  and no template contains "accepted" or "correct".

### 9.6 Judging

Every row below is one named test. **Pure** rows are table-driven over
injected records (no process, all three OSes); **real** rows run real judge
programs on Linux and macOS (§9 platform scope; Windows asserts the
refusal, status 10, for the same command lines). Each row asserts the
check, the test verdict, the coverage of every execution of the test, the
counts (§4.8) and the status (§5.2), and the derivation is repeated on the
same records to show it is deterministic.

#### 9.6.1 Batch judged tests (§2.11)

| Id | Fixture | Expected |
|---|---|---|
| J-B01 | validator skipped by intent set before the test | B1; solution, checker `skipped{cancelled}`; status by cancellation |
| J-B02 | role build failed: validator `skipped{build_not_succeeded}`; a `retention_budget` refusal earlier: `skipped{admission_closed}` | B2, `not_run{reason}` for each reason |
| J-B03 | validator `not_started{create_failed}`; another with stdout `write_error` | B3, `infrastructure`; later runs `skipped{input_not_validated}` |
| J-B04 | validator `not_started{cancelled_before_start}`; another stopped by intent | B4, `cancelled` |
| J-B05 | real `testlib` validator exiting 3, killed by its deadline, and dying by a signal | B5 each, `validation` outcome `validation_failed`, stderr kept |
| J-B06 | real `simple` validator exiting 1 | B6, outcome `invalid`; solution never started; status 5 |
| J-B07 | real `simple` validator exiting 2, by a signal, at its deadline | B7 with reasons `exit_code`, `signal`, `wall_deadline` |
| J-B08 | kind `compare` with a valid input; with a mismatch; with an invalid float answer | B8: exactly the §2.4/§2.5 results of the same test without a validator |
| J-B09 | intent after the validator, before the solution | B9 |
| J-B10 | solution `skipped{build_not_succeeded}`, `skipped{admission_closed}` | B10; checker skipped with the same reason |
| J-B11 | solution `not_started{setup}`; solution with cleanup `incomplete` | B11; checker `skipped{not_eligible}` |
| J-B12 | solution `transferred`; solution stopped by intent | B12; checker `skipped{cancelled}` or `skipped{not_eligible}` by §2.1 order |
| J-B13 | intent between the solution's record and the checker's admission | B13 |
| J-B14 | `retention_budget` refusal of an earlier run, then this test's checker | B14, `not_run{admission_closed}` |
| J-B15 | intent while a real checker runs (cancel during the checker); checker `not_started{cancelled_before_start}` | B15, check `unchecked{checking_interrupted}` |
| J-B16 | solution at its deadline, by a signal, exit 1, at its output cap, each with a checker that accepts | B16: the solution class, `diagnostic: true`, check `passed`, never success; with the checker `infrastructure`: `not_admitted{run_not_eligible}` |
| J-B17 | checker `not_started{create_failed}`, checker with cleanup `incomplete` | B17 |
| J-B18 | real `simple` checker exit 0 | B18 |
| J-B19 | real `simple` checker exit 1 | B19 |
| J-B20 | real `simple` checker exit 2, signal, deadline, own stdout cap (`Kill`) | B20 with reasons; never `wrong_answer`, never a zero score |
| J-B21 | real `testlib` checker by a signal, at its deadline, at its own output cap | B21; the report is not read even when present |
| J-B22 | `testlib` checker with cleanup `unverified` (injected) and a valid `accepted` report; `simple` checker exiting 0 with cleanup `unverified`; `simple` checker by a signal with cleanup `unverified` | B22, `unchecked`, status 4, report not read; the last B20 |
| J-B23 | report read failing with injected `EIO`; reservation refusal for the extracted report | B23: `tests[].check` `check_error`, `judges[0].result` `not_read{extraction_failed}`, `judges[0].report.path` `null`, `infrastructure` |
| J-B24 | report missing; a symlink to a valid report; a FIFO; mode `000` | B24 with each reason; the FIFO is never waited on |
| J-B25 | report of 65 536 bytes (valid) and 65 537 bytes | the first extracted (65 536 bytes) and decided by its content; the second rejected before extraction: B25, `judges[].report.path` `null`, never infrastructure |
| J-B26 | `encoding="koi8-r"`; undeclared cp1251 bytes; declared windows-1251 with byte `0x98`; invalid UTF-8 | B26 each |
| J-B27 | unclosed element; a `<!DOCTYPE>` with an entity; a processing instruction; `&nbsp;` | B27 each |
| J-B28 | root `verdict`; unknown `outcome`; `points` with `accepted`; `pctype` with `points`; a child element | B28 each |
| J-B29 | `points` = `1e3`, `-1`, `+1`, `1.0000000001` (10 fractional digits), 19 significant digits, raw = scale + 10⁻⁹ | B29 each; `0`, raw = scale, 18 significant digits and 9 fractional digits accepted (J-B37) |
| J-B30 | a valid `accepted` report with exit 4, 42, 255; `partially-correct` with `pctype` and exit `PC_BASE_EXIT_CODE + n` (both unsupported) | B30, `exit_unsupported` |
| J-B31 | `partially-correct` with `pctype` and exit 0; exit 7 + `points` under `pass_fail` | B31 with `outcome_unsupported`, `points_not_enabled` |
| J-B32 | exit 0 + `wrong-answer`; exit 1 + `accepted`; exit 7 + `fail` | B32, `disagreement` |
| J-B33 | real testlib checker: `quitf(_ok)` | B33 |
| J-B34 | `quitf(_wa)` with a cp1251 Cyrillic message | B34; message decoded to UTF-8, exactly 4 096 bytes kept when longer, `len` and `sha256` of the whole |
| J-B35 | `quitf(_pe)`; the header's unexpected-EOF presentation error | B35, `presentation_error`, status 1 |
| J-B36 | `quitf(_fail)`; `quitpi()` without points (header's unset-points failure) | B36, `declared_fail`, status 5 |
| J-B37 | `quitp(12.5)` on scale 100; `quitp(0)`; `quitp(100)` | B37, `scored`, raw canonical |
| J-B38 | orphans: checker `skipped{not_eligible}` after `exited_zero`; `skipped{input_not_validated}` after a valid input | `internal_error`, `result.internal.locus` test, 70 |
| J-B39 | paths with spaces and Unicode in the workspace path, real `simple` and `testlib` checkers | arguments are the absolute private-directory paths of E §2.1; `passed` |
| J-B40 | answer policy `none`: one answerless test, one with an answer | the first judge's `answer` file is empty, the second's equals the frozen answer; `tests[].answer` `absent` / `present` |
| J-B41 | injected `ENOSPC` while staging `output` | checker `not_started{setup}`, B17, `infrastructure`; staged bytes released |
| J-B42 | judge scratch: after the verdict the staged copies are gone and their bytes released; injected removal failure | `judge_scratch` warning, verdict unchanged |
| J-B43 | a role build failing (C++ checker with a syntax error) | later builds and every test run `skipped{build_not_succeeded}`, `build_failed`, status 3, label `CE` "checker build" |
| J-B45 | a real `testlib` checker writing a valid `accepted` report while the staged input and answer exceed `cwd_bytes` (configured small), and one that itself writes beyond `cwd_bytes` | the first: staged copies removed before measurement, no `CwdOverflow`, `passed`; the second: report read first, `passed`, then `CwdOverflow` and `cwd/` cleared (diagnostic only) |
| J-B46 | a checker opening a declared asset by a relative path | `passed`; the asset staged at its relative path and removed with the scratch |
| J-B47 | header provenance: one `testlib.h` beside the checker source (pinned bytes, other bytes); two captured `testlib.h`; one in another directory; none captured; a source with `#include <testlib.h>` (and `#  include <testlib.h>`) beside a captured header; a build with `-include other.h`, an `-I` to an external `testlib.h`, a conditional `#if 0` include and a wrapper compiler, each with a pinned candidate beside the source | `candidate` with `pinned` true / false; `not_identified{ambiguous}`, `{not_beside_sources}`, `{none}`, `{angle_include}`; the last four still `candidate` (the claim is candidate-only and the human output says "header candidate", never "compiled header"); a `testlib_header` warning except for a pinned candidate |
| J-B48 | each role's build failing (checker, validator, interactor), and each with `not_started{create_failed}`, with tests and with zero selected tests | later builds coverage `skipped{build_not_succeeded}`, build result `skipped{build_not_succeeded}`, every test execution `skipped{build_not_succeeded}`; job `build_failed` (3), respectively `infrastructure` (10); zero tests: the same, never `no_tests` |
| J-B49 | intent set while a role build runs (it is stopped), and intent set before a later build's admission after a failed build | the stopped build `cancelled`; every later build coverage `skipped{cancelled}` with build result `cancelled`, test executions `skipped{cancelled}` (cancellation precedes `build_not_succeeded`, §2.1); job `cancelled`, cancellation status; in the second case the failed build stays `failed` and the job is `cancelled` (row 3 precedes row 4) |
| J-B44 | intent after the checker finished | completed verdict, `cancellation.phase: primary`, fact `not_attempted{cancelled}` |

#### 9.6.2 Interactive tests (§2.12)

| Id | Fixture | Expected |
|---|---|---|
| J-I01 | pair skipped by intent before admission | I1, `cancelled` |
| J-I02 | pair `skipped{build_not_succeeded}`, `{admission_closed}`, `{input_not_validated}` | I2 (the last after B5–B7 decided) |
| J-I03 … J-I15 | one closed record per session-verdict row of §2.12, in table order (OK, WA, PE, score, TLE by wall, TLE by peer allowance, OLE, RE by signal, RE by exit, `session_limit`, `idle`, `CheckerFailed`, `Unchecked`) | that row's test verdict and score; `check: null`; `class` the solution's class |
| J-I16 | cancelled and infrastructure records | `cancelled`, `infrastructure`, no score |
| J-I17 | a record violating an invariant | `internal_error{internal_policy_error}`, `session.verdict` `internal_policy_error`, `result.internal.locus` test, 70 |
| J-I19 | cancel inside committed pair creation (injected gate): both runs `transferred` | I3: `cancelled`, `decided_by.kind = "transferred"`, facts `[cancel_requested]`, no step-0 error, follow-up checker `skipped{cancelled}`, cleanup reaps both later without changing the report |
| J-I20 | cancel inside the committed creation of the follow-up checker after I accepted | I3: `cancelled`, `decided_by` `transferred` role `checker`, c coverage `transferred` |
| J-I18 | follow-up checker `Started`, `NotStarted`, `CheckerSkipped{NotEligible}`, `CheckerSkipped{Cancelled}`; mode `interactor` | coverage `started`, `not_started`, `skipped{not_eligible}`, `skipped{cancelled}`; no checker execution planned |

#### 9.6.3 Reducer `interactive_v7` (checkers design §§6.1–6.2)

Pure, over constructed closed records, in every checking mode where the
row allows it. Ids cite the design's normative statements one by one.

| Id | Design statement | Expected |
|---|---|---|
| J-R01 | §6.1 `obs_seq` unique, record order | facts serialised in strictly increasing `obs_seq` |
| J-R02 | §6.1 `ValidationDone` attributed to its judge's `RootExited` | its `decision_pos` equals that `RootExited`'s |
| J-R03 | §6.1 `AuthorityComplete` attributed to its completing `ValidationDone` | equal `decision_pos` |
| J-R04 | §6.2 late-read report | I rejects; S's own-limit stop is observed after I's exit but before I's report is read → WA (I's exit precedes) |
| J-R05 | §6.1 fact set exhaustive | each of the ten fact kinds round-trips (encoder, decoder, Python oracle); any other kind is rejected by the decoder |
| J-R06 | §6.1 item 1 | exactly one `Started`/`NotStarted` per role; checker modes: one of `Started`/`NotStarted`/`CheckerSkipped`; mode `interactor`: no checker fact |
| J-R07 | §6.1 item 2 | one `StopRequested` per role, the first trigger |
| J-R08 | §6.1 item 3 | each teardown stop is preceded by its decisive fact, `CancelRequested` or `Infrastructure` |
| J-R09 | §6.1 item 4 | every started role has one `RootExited`, then one `CleanupResult` |
| J-R10 | §6.1 item 5 | every started judge has exactly one `ValidationDone`, after its `CleanupResult` |
| J-R11 | §6.1 rule 5.1 | judge `Stopped`, or `Natural` with a signal (and a `simple`-style exit outside 0/1) → `NotRead`; the stop or crash decides |
| J-R12 | §6.1 rule 5.2 | cleanup `Incomplete` → `NotRead`, verdict `infrastructure` |
| J-R13 | §6.1 rule 5.3 | cleanup `Unverified` → `Unverified`, nothing read, for the interactor and for a follow-up checker of either profile (a `simple` checker exiting 0 included); a crash with `Unverified` → `NotRead` (rule 5.1 first) |
| J-R14 | §6.1 rule 5.4 | `Confirmed` → `Ok(result)` or `Failed(class)`; exit 3 + `fail` → `Failed(DeclaredFail)`; I's `Ok` outside its row → `Failed(ProtocolUnsupported)`, never for `DeclaredFail` |
| J-R15 | §6.1 item 6 | the checker decision is taken once, after both `CleanupResult`s and I's `ValidationDone`: eligible → `Started`/`NotStarted`; `CheckerSkipped{Cancelled}` with a `CancelRequested`; else `CheckerSkipped{NotEligible}` |
| J-R16 | §6.1 item 7, mode `interactor` | `AuthorityComplete` iff `ValidationDone{I, Ok(Passed \| Scored)}`, immediately after it, with its result |
| J-R17 | §6.1 item 7, checker modes | `AuthorityComplete` iff I `Ok(Passed)` and checker `Ok(r)`, with r |
| J-R18 | §6.1 item 8 | one `Infrastructure` fact; later conditions increment `suppressed` (saturating `u32`) |
| J-R19 | §6.1 bound, per fact | the largest instance of every variant: ≤ 512 bytes without text; each text ≤ 256 bytes after escaping, `truncated: true`, cut at an escape boundary |
| J-R20 | §6.1 bound, record | 17 maximal facts ≤ 13 056 bytes ≤ 16 KiB; ≤ 17 facts in every generated record |
| J-R21 | §6.1 canonical `Scored` | ≤ 20 characters, no leading or trailing zeroes |
| J-R22 … J-R37 | §6.1 same-wakeup order: the 16 adjacent pairs of `NotStarted` < `RootExited` < `CleanupResult` < `ValidationDone` < `AuthorityComplete` < `CheckerSkipped` < `Started` < `StopRequested` (`OwnLimit(Wall)` < `OwnLimit(DialogueOutput)` < `PeerAllowanceExpired` < `SessionLimit` < `Idle` < `Teardown(PeerDecided)` < `Teardown(Cancel)` < `Teardown(Infrastructure)`) < `CancelRequested` < `Infrastructure` | each pair delivered in one wakeup in reverse order is recorded in the stated order |
| J-R38 | §6.1 role order | S < I < checker, two pairs |
| J-R39 | §6.1 prerequisites | a fact delivered in the same wakeup as its prerequisite is recorded after it (including `NotStarted{CancelledBeforeStart}` after `CancelRequested`) |
| J-R40 | §6.1 exits before timers | an exit available at a timer wakeup is ordered first |
| J-R41 | §6.1 item 2 + order | wall and dialogue cap of one role in one wakeup → only `OwnLimit(Wall)` |
| J-R42 | §6.1 closure, determinism | reduction only on a closed record; the same record, mode and scoring always give the same verdict |
| J-R43 … J-R53 | §6.2 decisive table, one per row in table order | that row's verdict |
| J-R54 | §6.2 not decisive: `Teardown` stops | no effect on the verdict |
| J-R55 | §6.2 not decisive: `ValidationDone{_, NotRead}` | no effect |
| J-R56 | §6.2 not decisive: `Ok(_)` other than I's rejection | no effect without `AuthorityComplete` |
| J-R57 | §6.2 not decisive: `NotStarted{_, CancelledBeforeStart}` | `cancelled` through steps 3–4 only |
| J-R58 | §6.2 not decisive: `CheckerSkipped` | no effect |
| J-R59 | §6.2 secondary I exit after a decisive S fact | evidence only |
| J-R60 … J-R67 | §6.2 step 0, one mutation each: an emission item violated; `CheckerSkipped` inconsistent; `ValidationDone` inconsistent; `AuthorityComplete` missing or unjustified; `Stopped(n)` not naming the role's stop; `CancelledBeforeStart` without an earlier `CancelRequested`; two decisive facts at one `decision_pos`; 18 facts | `internal_policy_error`, never success |
| J-R68 | §6.2 step 1 | `Infrastructure` decides even after an earlier decisive fact |
| J-R69 | §6.2 step 2 | of two decisive facts the smaller `decision_pos` decides |
| J-R70 | §6.2 step 3 | `CancelRequested` before d → `cancelled`; after d → d's verdict |
| J-R71 | §6.2 step 4 without cancellation | `AuthorityComplete{Passed}` → OK, `{Failed}` → WA/PE, `{Scored}` → score |
| J-R72 … J-R96 | §6.2 coverage table, one per row in table order (25 rows) | that row's verdict |
| J-R97 … J-R111 | §6.2 cancellation in every mode (`interactor`, `checker`, `interactor+checker`) × {before d, after d, no d and before execution completion, execution complete but before `AuthorityComplete`, after both} | `cancelled`, d's verdict, `cancelled`, `cancelled`, the `AuthorityComplete` result |
| J-R112 … J-R117 | §6.2 every `CheckerFailClass` | `DeclaredFail`, `ProtocolUnsupported`, `ReportInvalid`, `ArtefactInvalid`, `Crashed` through a session; `TestDataError` through the validator before a session (B5) |
| J-R118 | §4, §6.3 points under `pass_fail` | I's points → `CheckerFailed{ProtocolUnsupported}`, `outcome_unsupported` |
| J-R119 | §6.3 I's score under `checker` or `interactor+checker` | `CheckerFailed{ProtocolUnsupported}`, `outside_authority`, attributed to I's exit |
| J-R120 | §6.3 the two checker modes | the same records reduce identically |
| J-R121 | property: records generated from the emission contract over a small model (all interleavings) | never step 0, always a defined verdict |
| J-R122 | property: a record mutated to violate exactly one invariant | always step 0 |
| J-R123 | property | no `passed` or score without `AuthorityComplete` |
| J-R124 | property | fact count ≤ 17 and serialized size ≤ 16 KiB |
| J-R125 | §6.2 completeness | `checking_interrupted` is never produced for a session |
| J-R126 | §6.1 rule 5.4 as amended (§10) | `ValidationDone{_, NotRead}` after a `Confirmed` cleanup of a naturally exited judge with an `Infrastructure{judge_extraction}` fact in the same wakeup → valid, `infrastructure`; with an earlier `Infrastructure` fact (from S's cleanup) and that fact's `suppressed` incremented, no second fact → valid, `infrastructure`, fact count within 17; with no `Infrastructure` fact at or before it → step 0, `internal_policy_error`; a record with both `Started` and `CheckerSkipped` for the checker → step 0 |

#### 9.6.4 Session producers (real, Linux and macOS)

| Id | Fixture | Expected |
|---|---|---|
| J-P01 | real testlib interactors: adaptive guess-the-number (accepts), one that rejects, one that scores | OK, WA, score; `session.facts` closed and within the bound |
| J-P02 | interactor deletes `tout`; writes 64 MiB + 1 byte to it; replaces it by a symlink; sets its mode to `000` | rejected before extraction: `checker_failed`, `artefact_invalid`, `tout_missing` / `tout_too_large` / `tout_not_regular` / `tout_unreadable`, attributed to the interactor; nothing extracted, no infrastructure |
| J-P03 | interactor cleanup `unverified` (injected); a `simple` follow-up checker exiting 0 with cleanup `unverified` | `unchecked{checking_unverified}`, `tout` and report not read; for the checker, no `AuthorityComplete` |
| J-P04 | follow-up checker, mode `checker` | receives `tout` (copy) in the output position, its own profile arguments |
| J-P05 | solution TLE while the interactor waits; the interactor then exits on EOF with WA | TLE |
| J-P06 | solution dies by `SIGPIPE` after the interactor accepted, `early_reader_close` true and false | OK with `sigpipe_normalised: true`; RE otherwise |
| J-P07 | peer allowance expiry each way; `idle_s = 1` with both roots sleeping | TLE "did not exit after the interactor finished" / `checker_failed{crashed}`; `idle` with the `idle` member |
| J-P08 | direct mode | dialogue counters `null`, `transcript.kind = "unavailable"`, `idle_s` refused by configuration |
| J-P09 | seeds `env`, `argv`, `none`, and `solution_seed = "env"` | each value in that role's environment or last argument only, recorded in `session.seeds`; `none` recorded as not seed-controlled |
| J-P10 | transcript of a passed test, of a failed test, with gaps | `removed`, `complete`, `gapped` with exact counts; reservation 35 684 864 B per session converted at release |
| J-P11 | seeded rerun of a failing session, deterministic and nondeterministic | `rerun.reproduced` true / false |
| J-P12 | `cpf interactor-test`: a passing `good` list; a `bad` solution that passes | success; the command fails |
| J-P13 | interactive, `cwd_bytes` configured small: an interactor writing a valid `accepted` report and a large `tout` (mode `interactor`), and the same with a follow-up checker (mode `checker`) | `tout` and report extracted to `judge/` before the interactor's `cwd/` is measured and cleared (`CwdOverflow` diagnostic); OK in mode `interactor`; in mode `checker` the checker receives the extracted `tout` and decides; injected extraction failures, each → verdict `infrastructure`, the closed record passes step 0, no `AuthorityComplete`, never `artefact_invalid` or `internal_policy_error`: (a) mode `interactor`, reservation refusal for the interactor's `tout` → `ValidationDone{interactor, NotRead}` reason `extraction_failed` + `Infrastructure{judge_extraction}`, no checker fact; (b) mode `checker`, write error for the interactor's report → the same, and the checker `CheckerSkipped{NotEligible}` (coverage `skipped{not_eligible}`); (c) mode `checker`, intent set before (b)'s checker decision → `CheckerSkipped{Cancelled}`, verdict still `infrastructure` (step 1); (d) mode `checker`, interactor accepted, the follow-up checker started and its report extraction fails → the checker keeps `Started`, `RootExited`, `CleanupResult`, gets `ValidationDone{checker, NotRead}`, no `CheckerSkipped`; (e) S's cleanup `Incomplete` already recorded `Infrastructure`, then the interactor's extraction fails → no second fact, `suppressed` = 1 |

### 9.7 Stress

| Id | Fixture | Expected |
|---|---|---|
| R-S01 | §2.14 derivation table: every row with the minimal iteration set that reaches it (pure, all OSes) | the verdict and the §5.2 status: 70, 10, cancellation, 3, 10, 1, 5, 5, 5, 5, 4, 0 |
| R-S02 | `irid` vectors: `0.generator`, `18446744073709551615.candidate_interactor` | accepted, 43 bytes serialized at the maximum; rejected: `01.candidate`, `1.Candidate`, `18446744073709551616.brute`, `1.`, `.brute`, `r00001` (as `irid`) |
| R-S03 | `fid` vectors (stress contract §10.3 ST-F02) | as there, 78 bytes serialized at the maximum |
| R-S04 | worst-case `stress` member: 8 classes × 16 diagnostics of 1 024 `"`-heavy bytes, 20 failure refs with 8 infrastructure-class runs each (8 causes) and `not_promoted` with a 260-byte message, 16 cleanup paths of 256 bytes, every `u64` at `u64::MAX` | ≤ 606 208 B; the whole report with 6 builds at their worst ≤ B_S; the reservation equals 2 × B_S |
| R-S05 | `persist` warning for an iteration run with an `irid` at its maximum and an `io` error at its maximum | ≤ 768 B |
| R-S06 | `stress_failures` entry at its maximum; 10 001 entries | ≤ 1.5 KiB; 10 000 listed, `entries_dropped` 1 |
| R-S07 | stalled-then-resumed stress case (portable, injected engine; §9.3): 10 000 iterations, `--keep-going`, 3 failing iterations, `stderr.write` blocked from the first iteration and released 1 s after t_q | stderr holds, in order, the start-up diagnostics, the stress result lines (each present exactly once, in the §8 order), the warnings with `progress_dropped.lines` > 0, the summary last; peak queued result bytes within the final allowance; Linux and macOS repeat it with real fixture programs |
| R-S08 | `--events` on a stress job of 10 005 iterations with the consumer stalled, then released | `job_finished.seq` equals the number of earlier attempts; the gap equation holds; `iteration_run_*` events carry `irid`, none carries `rid` |
| R-S09 | `--events=output` on a stress job past iteration 10 000; a `cwd_overflow` and a `judge_scratch` warning from iteration 10 003 | `output_chunk.run` = `"10003.candidate"` etc.; the warnings' `run` is that `irid`; each within its bound (event line ≤ 7 KiB, warning ≤ 768 B) |

### 9.8 File-based I/O

Real rows run real programs through `cpf test` on Linux and macOS (the
macOS runner's default case-insensitive APFS volume); on Windows the same
command lines assert exit 10 before admission and no job directory.
Injected rows use the execution-contract injection points and run on all
three OSes where no process is needed. Each row asserts `tests[].check`,
`tests[].verdict`, `tests[].class`, `tests[].output_file`, `target.io`,
the coverage of every execution of the test, the counts (§4.8) and the
status (§5.2); the configuration rows are C §11.

| Id | Fixture | Expected |
|---|---|---|
| F-01 | `io = { input = "in.txt", output = "out.txt" }`; a C++ program that reads `in.txt`, writes the answer to `out.txt`, and also reads stdin and writes the number of stdin bytes it read to stderr | `passed`; `out/stderr` says 0; `target.io` `{input: "in.txt", output: "out.txt", stdin: "null_device"}`; `output_file` `collected` with the answer's bytes and SHA-256, `path` `<run>/out/output`; `cwd/` holds neither `in.txt` nor `out.txt` after the run |
| F-02 | `io = { output = "out.txt" }`; the program reads stdin, writes `out.txt` | `passed`; `stdin: "test_input"` |
| F-03 | `io = { input = "in.txt" }`; the program reads `in.txt`, writes stdout | `passed`; `output_file` `null`; stdout compared as without `io` |
| F-04 | `io = { input = "in.txt", output = "out.txt" }`; the program reads only stdin (`while (cin >> x)`) and writes what it read to `out.txt` | it ends at once on end-of-file (well inside the deadline), `wrong_answer`; never blocks |
| F-05 | output file and stdout: (a) correct answer in `out.txt` and 1 MiB of other bytes on stdout; (b) correct answer on stdout and no `out.txt`; (c) the answer in both | (a) `passed`, `executions[].summary.stdout.stored` = 1 048 576 and `out/stdout` holds those bytes; (b) `output_rejected{output_missing}`, `wrong_answer`, status 1; (c) `passed` |
| F-06 | no output file, exit 0, with an answer and with an answerless test; no output file and exit 3 | `output_rejected{output_missing}`, `wrong_answer` for both (never `unchecked{no_answer}`); exit 3: `nonzero_exit`, the check `output_rejected` with `diagnostic: true` |
| F-07 | declared `out.txt`, the program writes `OUT.TXT`; declared `résultat.txt` (NFC), the program writes the NFD spelling | `misnamed`, `output_rejected{output_misnamed}`, `wrong_answer` on Linux and on macOS alike (on macOS the other spelling would open) |
| F-08 | `stdout_cap = 4096`; output files of 4 095, 4 096 and 4 097 bytes, each the correct answer as far as it goes | 4 095 and 4 096: `collected`, compared, `passed`; 4 097: `too_large{bytes: 4097, budget: 4096}`, test class `output_budget{stream: output_file}`, check `output_rejected{output_too_large}` diagnostic, `output_budget`, `executions[].summary.class` `exited_zero`, nothing in `out/output`; the reservation of `out/output` returned |
| F-09 | an output file that grows past the bound between `fstat` and the read (injected read size) | `too_large{bytes: budget + 1}`, partial copy removed, `output_budget` |
| F-10 | `out.txt` a symbolic link to a valid answer inside `cwd/`; to `/etc/hosts`; to a FIFO nobody writes | `not_regular{link}`, `output_rejected{output_not_regular}`, `wrong_answer` each; the collection's test-only operation log (an injection point recording every `openat`/`fstatat` with its name and flags) shows `fstatat(AT_SYMLINK_NOFOLLOW)` on `out.txt` and the confirming listing, and no open of `out.txt` or of any link target |
| F-11 | `out.txt` a directory; a FIFO; a Unix-domain socket; a regular file replaced by a socket between the type check and the open (injected) | `not_regular{directory}`, `not_regular{other}` three times; `wrong_answer`; never blocks, never `infrastructure` |
| F-12 | the program writes `out.txt` and sets its mode to `000`; the program sets the mode of its `cwd/` to `000` (CI user not root; a root runner skips with a recorded reason) | `unreadable{os_error: EACCES}`, `wrong_answer`, both |
| F-13 | injected `EIO` on the read after a successful open; injected `ENOSPC` while writing `out/output` | `not_collected{io}`, `check_error{os_error}`, `infrastructure`, status 10; partial copy removed |
| F-14 | empty `out.txt` with an empty answer; with answer `1` | `collected{bytes: 0}`, `passed`; `wrong_answer` with a comparator `diag` |
| F-15 | kind `checker`: a `testlib` checker that accepts iff its `output` file equals its `answer` byte for byte, with the answer written to `out.txt` and garbage on stdout; then no `out.txt` | `passed` (the checker received the file, not stdout); then the checker `skipped{not_eligible}`, B14a, `wrong_answer`; with exit 1 and no `out.txt`, B16 with the diagnostic collection check, `nonzero_exit` |
| F-16 | a `testlib` validator with `io.input`; the validator exits 3 on one input | the validator reads its own staged `input` (E §2.1) on stdin; the valid test `passed`; the other B5, `validation_failed`, the solution `skipped{input_not_validated}` and never admitted, so nothing is staged for it |
| F-17 | `cwd_bytes = 1 MiB`; the program writes a correct `out.txt` and 2 MiB of other files; a 2 MiB staged input with a correct small `out.txt` | the first: collected before clearing, `passed`, then `CwdOverflow` (`retention` warning); the second: no `CwdOverflow` (the staged input is not counted) |
| F-18 | the program deletes `in.txt`; replaces it by a directory; by a symbolic link to `/etc/hosts`; renames it to `x.txt` | `passed` each when the output is correct; no warning; the link removed as an entry (its target untouched); the directory and `x.txt` left as program output; the staged input reservation released in every case |
| F-19 | the program overwrites `in.txt`; two tests; the same job run twice | the frozen `inputs/<id>.in` and `input_sha256` unchanged; each run starts with the frozen bytes |
| F-20 | injected `ENOSPC` while staging `in.txt`; `job_bytes` computed by the test from the plan so that freezing, the report reservation and the first run's reservations without the staged copy fit and with it (the frozen input's size more) do not | `not_started{setup}`, `infrastructure`, nothing started; `not_started{retention_budget}` for the first solution run (freezing succeeded), later runs `skipped{admission_closed}` |
| F-21 | injected removal failure for the staged input and for the collected original; repeated on every test of a job whose `job_bytes` fits only a few kept copies; an unchanged staged input deleted by the program | `io_scratch` warnings `staged_input_kept`, `output_kept`; verdict unchanged; the staged size excluded from the `cwd_bytes` measurement; the job's charged bytes still include each kept staged copy (accounting interface counters), so a later run is `not_started{retention_budget}` and charged bytes never exceed `job_bytes`; the program-deleted copy is released |
| F-22 | cancellation intent set after the solution's record and before its check | `output_file` `collected` (collection is not a check), check `not_admitted{cancelled}`, `cancelled` |
| F-23 | cleanup `unverified` (injected) with a correct `out.txt` | `passed`, "cleanup not verified" |
| F-24 | stdout `Kill` breach (`stdout_cap = 4096`, 8 KiB on stdout) with a correct `out.txt`; stdout with `on_exceed = "discard"` and the same | `output_budget{stream: stdout}` (`Kill` still stops); with `Discard`, note `truncated_stdout` and `passed` |
| F-25 | `io = { input = "in.txt", output = "out.txt" }`; a program that renames `cwd/` to `old/` inside its run directory and creates a new `cwd/` holding a different `out.txt` before exiting | the `out.txt` of the directory it was started in (now `old/`) is judged (H, E §5.6); the staged `in.txt` is removed from `old/` through H and its charge released (accounting counters); measurement and clearing skipped with `io_scratch` `not_measured`; no H outstanding afterwards (descriptor count at quiescence) |
| F-26 | raw `cpf run`, `cpf cmd` with `for_each_test` and `cpf build` on an `io` problem | nothing staged (an empty `cwd/`), stdin as for those commands, no collection, `target.io` `null` and no `output_file`; `cpf config show` still shows `config.io` |
| F-27 | worst-case sizes: `target.io` with the distinct 255-byte names `é` × 127 + `a` and `é` × 127 + `b` and `io.input` (562 bytes with key and comma), every `output_file` variant at its maximum, `tests[].class` `output_budget` with 3 streams | each ≤ its §3.7 row; kind `compare` with `io.output` at the cap reserves 254 982 144 B |
| F-28 | `io` on an interactive problem; `cpf stress` on an `io` problem; Windows `cpf test` on a valid `io` problem | 7 at `config.io`; 7 (stress contract); 10 before admission, no job directory |
| F-29 | a barrier-controlled descendant (survives the root; cleanup `unverified` injected): (a) renames `out.txt` to `OUT.TXT` between the pathname observation and the confirming listing, and keeps it; (b) replaces `out.txt` by a new file (another inode) at the same point; (c) renames `out.txt` to `OUT.TXT` at the same point and back to `out.txt` right after the listing; (d) after extraction, puts a new file (another inode) at `out.txt` and appends to the accepted inode | (a) no byte-equal entry in the listing → `misnamed` on Linux and macOS alike; (b) inode mismatch → next attempt, then `collected` with the new file's bytes; (c) `misnamed`, decided by attempt 1's listing (a spelling outcome is never retried); (d) `out/output` and its `sha256` unchanged by both changes; the barrier points are recorded by the test-only operation log |
| F-30 | (real, Linux and macOS) `cwd/` with 65 537 entries created by the program and no `out.txt`; (stub listing, all OSes) a listing order with `out.txt` at position 65 537, and at position 65 536 | real: `too_many_entries`, `output_rejected{output_dir_too_large}`, `wrong_answer` (deterministic: the name is absent, so the bounded read is cut off without finding it); stub: `too_many_entries` at 65 537, `collected` at 65 536 |
| F-31 | `stdout_cap = 4096` (deadline: the run's sink finalisation deadline, 5 s from the drain milestone): (a) a stub filesystem stalling the confirming listing, with cancellation intent set during the stall; (b) the same stall without intent; (c) a stalled extraction write; (d) the output-file worker refused at admission (thread credits exhausted) | (a) `output_file` `not_collected{timeout}` recorded by the deadline, but intent preceded the check: check `not_admitted{cancelled}`, verdict `cancelled` (§2.4 step 3); (b), (c) `not_collected{timeout}`, `check_error`, `infrastructure`; in (a)–(c) the record is finalized by the deadline, the worker is transferred, removes its partial copy itself when the stub releases it, and only then do the credit and reservations return (counters); the cleanup poller performs no file operation (test-only operation log); the `cwd/` step is skipped with `io_scratch` `not_measured`; (d) `not_started{cleanup_capacity}` or `not_started{worker}`, infrastructure |
| F-32 | `stdout_cap = 4096`; a file of 4 000 bytes at the `fstat` that a barrier-controlled descendant grows to 8 192 bytes before the copy ends; the same with an injected failure to remove the partial copy | the operation log shows the 1-byte probe read into memory after 4 096 copied bytes; the extracted copy's on-disk length never exceeds 4 096 (accounting counter maximum) and the job's charged bytes never exceed the reservation; `too_large{bytes: 4097, budget: 4096}`, `output_budget`; with the removal failure, `io_scratch` `partial_copy_kept` and the 4 096 bytes stay charged until the job's owner is released |
| F-33 | a stub filesystem blocking the staged-input write, cancellation intent set during it; the same block released after the job ended | the run `not_started{cancelled_before_start}` (published without waiting for the creation thread, E §3.4), later runs `skipped{cancelled}`, the report produced; the staged file and its charge stay with the creation thread until it returns and removes the file, or until the job's owner is released; nothing else touches it |
| F-34 | wrongly spelled `OUT.TXT` (for `out.txt`) as a symbolic link, as a directory, as a mode-`000` regular file; a mode-`000` `out.txt` renamed to `OUT.TXT` between the pathname observation and the confirming listing | `misnamed` in every case on Linux and on macOS (case-insensitive APFS), never `not_regular` or `unreadable`; the link is never followed and nothing is opened |

## 10. Amendments to other documents

Every amendment text this contract prescribed for the execution, storage
and configuration contracts (gate 1, judging and file-based I/O) and for
`src/exit.rs`, `src/error.rs` and the release profile is applied in those
documents and files. The texts prescribed for the design documents and the
compare spec remain, to be applied by the design ratification:

- **Checkers design** (CLI exit codes "belong to the report transport"): no
  text change; §5 provides them.
- **Workspace design §3.2**, add the judge fingerprint (configuration
  contract §9.1.3) to the run fact and its comparison in `status` (§6.1
  `judge`).
- **Compare spec §8**, replace "needs its checker (initial feature
  release)" by "needs its checker (configuration contract §9.1)".
- **Checkers design §6.1 rule 5** (needs ratification): add to rule 5.4
  "if reading or extracting the judge's report or `tout` fails on cpf's
  side (a reservation refusal or an I/O error on an artefact that passed
  the presence, type and size checks; execution contract §2.1), the
  outcome is `NotRead` and an `Infrastructure` fact is recorded in the same
  wakeup (item 8), or, when the record already has its one
  `Infrastructure` fact, that fact's `suppressed` count is incremented and
  no second fact is recorded"; step 0 accepts `NotRead` after a
  `Confirmed` cleanup of a naturally exited judge iff the record has an
  `Infrastructure` fact at or before that `ValidationDone`'s wakeup. Item 1
  and item 6 are unchanged: only a planned follow-up checker that never
  started gets `CheckerSkipped` (`Cancelled` before `NotEligible`).
  The fact payload's detail kind gains `judge_extraction`.
- **Checkers design §2.1** (needs ratification, the design being frozen):
  "records the hashes of its source and of the header it compiled
  against" is replaced by "records the hashes of its source and of a
  **candidate** header: the single captured `testlib.h` beside every
  source, which a quoted include would resolve to first; the candidate is
  not proof of the header compiled, and without one the reason is
  recorded" (execution contract §2.1), since cpf does not observe the
  compiler's file accesses and captures no build trace.
- **Design §6.2** (Supervisor), after "files a program
  writes itself are only monitored best-effort (no quota promise)" add:
  "the one exception is the declared output file of a file-based I/O
  problem, which cpf reads back under the stdout cap (a read bound, not a
  write quota; execution contract §5.6)".

## 11. Decisions

Decided (user, 2026-11-21):

- **D1** Exit codes 0–11 plus 70 (§5.1); code 12 (`site`, `cpf fetch`)
  was added with the commands documentation.
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

Settled with the judging amendment and kept by the schema-1
reconciliation (§3.6):

- **J1** Judging extends schema 1 in place (§3.6), no schema 2.
- **J2** At most 1 000 judged tests per job, and no plan with a report
  reservation above 256 MiB (C §8.2): with 4 KiB messages, 1 KiB
  diagnostics and 16 KiB sessions a judged test costs up to 83 KiB with
  its executions; the test cap keeps ordinary judged plans far below the
  reservation cap, which bounds every plan, hook-heavy ones included.
- **J3** Kind `compare` keeps `unchecked{no_answer}` for an answerless
  test; the `answer` policy applies to judges only (C §9.1.4).
- **J4** The three answer states are encoded without a new `tests.toml`
  key: omitted, zero-byte, and a missing file as an unhealthy entry
  (C §9.1.4).
- **J5** A checker still runs after a solution failure, as a diagnostic
  that never decides (§2.11 B16), as comparison does.
- **J6** The validator runs first; a test whose input is not valid admits
  no other run (§2.1 `input_not_validated`).
- **J7** A batch `testlib` checker with cleanup `Unverified` is not read
  (`unchecked`), the rule checkers design §6.4 states for sessions.
- **J8** Builds run in order and a failed build skips the later ones (§2.3).
- **J9** An interactive test has `check: null`; its decision is the
  session (§2.12); `internal_policy_error` is an internal failure (70).
- **J10** Seeds are drawn per session from the OS random source, decimal
  `u64` (C §9.1.6); the display profile is a per-problem `[check]` key.
- **J11** A judge's report and `tout` are extracted to `judge/` outside
  `cwd/` before the cwd measurement; staged copies are removed right after
  that extraction, and the transcript and extracted `tout` of a passed test
  once its verdict is derived; extracted reports are always kept (E §2.1).

No open decisions remain.

## Appendix A. Schema-1 inventory (implementation checklist)

Every schema-1 document kind, member, enum value and exit row, as defined
in §§1–8 and the contracts they cite. It is an index, not a second
definition: where it and a section differ, the section governs and the
difference is a defect of this appendix. The implementation of the report
schema (encoder, decoder, Python oracle, bounds, exit mapping) implements
every entry, including those only later producers emit (stress,
submission, archive, fetch), each with an encoder vector and an oracle
round trip. Member lists are in wire order. `T?` is nullable, `[T; n]` a
capped list followed by `<name>_dropped`, `[T; ≤n]` a bounded list.

### A.1 Document kinds

| `document` | Written by | Persisted | Defined | Bound (§3.7.2) |
|---|---|---|---|---|
| `job_report` (`outcome` `finished`) | `cpf build`, `test`, `cmd`, `stress`, `submit-prep` (stage mode); `hook` jobs (report file only); raw `cpf run` (report file only, E §8.1) | `report.json` | §4 | B, H by kind; B_S |
| `job_report` (`outcome` `refused`) | the same job commands in machine mode | never | §4.12 | 104 KiB |
| `capture_manifest` | every job with a source-consuming stage | `inputs/capture.json` | §4.2 | exact |
| `event_stream` (header line), then event envelopes and the final `report` envelope | job commands with `--events` | never | §7 | 7 KiB per line; final envelope bound + 28 |
| `error` | non-job commands that fail in machine mode | never | §4.12 | 80 KiB |
| `config_show` | `cpf config show --json` | never | C §10 | none |
| `status` | `cpf status --json` | never | §6.1 | none |
| `ls` | `cpf ls --json` (also `--archived`, `--contests`) | never | §6.1 | none |
| `search` | `cpf search --json` | never | §6.1 | none |
| `notes` | `cpf notes --json` | never | §6.1 | none |
| `stress_failures` | `cpf stress ls --json` | never | §4.15 | 14 MiB |
| `submit` | `cpf submit-prep --json`, source-copy mode | never | §6.1, submission contract §9.2 | 35 KiB |
| `archive_plan` | `cpf archive --json` (plan mode) | never | §6.1 | none |
| `archive` | `cpf archive --apply --json`, `--resume --json` | never | §6.1 | none |
| `restore` | `cpf restore --json` | never | §6.1 | none |
| `fetch` | `cpf fetch --json` | never | §6.1, sites contract §11.2 | none |

Every document starts `{"schema":1,"document":"<kind>"` and has
`platform` (`unix`, `windows`) (§3.1, §3.3).

### A.2 Field types (§3.2)

`u32`, `i32`, `code`, `u64`, `i64`, `dur`, `bool`, `null`, `time`,
`hex64`, `hex32`, `token`, `rid`, `irid`, `fid`, `jid`, `ident`, `pident`
(local and remote), `ver`, `rel`, `text`, `dec`, `ns` (`utf8` |
`bytes_b64`), `nsum(P)` (`head`, `len`, `truncated`, `sha256`), `bytes`
(`bytes_b64`), `score`; `dec` is {`text`, `bits`}. Reader rejections:
§3.5.

### A.3 `job_report` members

Top level (§4.1): `schema`, `document`, `platform`, `outcome` (`finished`,
`refused`), `cpf_version`, `command` (`build`, `test`, `run`, `cmd`,
`hook`, `stress`, `submit_prep`), `job`, `workspace`, `problem`, `target`,
`capture`, `fingerprints`, `build`, `roles`, `executions`, `tests`,
`aggregates`, `result`, `fact`, `submission`, `hooks` + `hooks_dropped`,
`warnings` + `warnings_dropped`, `events`, `timing`, `refusal`, `stress`,
`persistence` (stdout document and final envelope only).

- `job` (§4.2): `id`, `dir`, `sequence` {`epoch`, `n`}, `kind` (job kinds
  `build`, `test`, `run`, `cmd`, `hook`, `stress`, `submit`).
- `problem`: `identity`, `name`, `kind`, `dir`.
- `capture` and the capture manifest (§4.2): `check`, `sources` +
  dropped, `support_files` + dropped, `counts` {`sources`,
  `support_files`}, `bytes`, `lists_sha256`, `manifest`; manifest members
  `job`, `check`, `sources`, `support_files`, `lists_sha256`, `roles`
  [{`role`, `files`}]. `Check`: `checked`, `unchecked{reasons}` (reason
  `shell_stage`).
- `target` (§4.3): `name`, `language`, `profile`, `build_free`,
  `compare`, `check`, `io`, `run_deadline` {`us`, `source`: `configured`,
  `derived`, `default`, `cli`}, `published_limits` {`time_ms`,
  `memory_bytes`}, `local_limits`. `compare`: `tokens`, `exact`, `lines`,
  `float` {`abs`, `rel`, `rel_base` (`answer`, `max`, `max1`), `combine`
  (`either`, `both`), `ulps`, `int_exact`}. `check`: `kind` (`compare`,
  `checker`, `interactive`), `answer` (`file`, `none`), `scoring`
  (`pass_fail`, `points` {`scale`, `aggregation`: `sum`, `min`}),
  `display` (`cpf`, `judge_cf`), `interactive` {`checking` (`interactor`,
  `checker`, `interactor_checker`), `mode` (`relay`, `direct`), `idle_us`,
  `early_reader_close`, `interactor_allowance_us`, `peer_allowance_us`,
  `solution_seed`, `interactor_seed` (`none`, `env`, `argv`)}. `io`
  {`input`, `output`, `stdin` (`null_device`, `test_input`)}.
  `LocalLimit`: `kind` (`address_space`, `job_commit`), `configured`,
  `installed`, `mechanism`, `outer` (`none`, `known{bytes}`, `unknown`).
- `fingerprints`: `source`, `tests`, `judge`.
- `build` (§4.4): `execution`, `build_id`, `output_root`, `result`,
  `artifacts` + dropped. `BuildResult`: `skipped{reason}`
  (`build_not_succeeded`, `admission_closed`, `submit_not_succeeded`),
  `succeeded`, `cancelled`, `infrastructure{causes, causes_dropped}`,
  `failed{cause}`, `internal_error{invariant}`. `BuildCause`: a class
  other than `infrastructure`/`cancelled`, `missing_output{names,
  names_dropped}`, `generated_program_invalid{name, reason}`,
  `output_too_large{bytes, cap}`.
- `roles[]` (§4.4): `role` (`checker`, `validator`, `interactor`,
  `generator`, `brute`), `profile` (`testlib`, `simple`, `null`),
  `origin` (`built{language, entry}`, `prebuilt{program}`), `build`,
  `capture` (as `capture` without `manifest`, plus `content_sha256`),
  `provenance` (`built{header}`, `declared`). `Header`:
  `candidate{path, sha256, pinned}`, `not_identified{reason}` (`none`,
  `ambiguous`, `not_beside_sources`, `angle_include`), `not_applicable`.
- `executions[]` (§4.5): `index`, `stage` (`build`, `run`, `command`,
  `hook`, `checker`, `validator`, `interactor`, `submit`,
  `submit_probe`), `role` (`build`, `test{test}`, `command{test,
  iteration}`, `raw`, `hook{name, trigger}`, `role_build{role}`,
  `checker{test}`, `validator{test}`, `interactor{test}`, `submit`,
  `submit_probe`), `coverage` (`started`, `transferred`,
  `not_started{reason}`, `skipped{reason}`, `admitted`), `run`,
  `program`, `spec`, `record`, `summary`, `class`, `invariant`
  (`record_missing`, `summary_underivable`, orphan invariants).
  `skipped.reason`: `build_not_succeeded`, `admission_closed`,
  `stop_policy`, `cancelled`, `job_not_eligible`, `submit_not_succeeded`,
  `input_not_validated`, `not_eligible`. `NotStarted`:
  `cancelled_before_start`, `create_failed{api, os_error}`,
  `limit{os_error}`, `unsupported_limit`, `unsupported`,
  `containment_unavailable`, `setup{os_error}`, `cleanup_capacity`,
  `worker`, `retention_budget`, `spec_too_large`. `Ref`: `digest`,
  `path`, `error`. `PersistError`: `timeout`, `queue_full`, `cancelled`,
  `worker_unavailable`, `io{os_error, message}`.
- `RunSummary` (§4.5): `class`, `execution`, `timing` {`setup_us`,
  `wall_us`, `observer` (`pidfd`, `kqueue`, `polling`,
  `process_handle`)}, `deadline_us`, `stdout`, `stderr` {`observed`,
  `stored`, `forwarded`, `eof`, `complete`, `exceeded_at`, `losses`,
  `file`}, `measurements`, `limits`, `supervisor_errors`, `notes` +
  dropped, `cwd_overflow_bytes`. Class: `exited_zero`,
  `nonzero_exit{code}`, `signal{signal, name, core_dumped}`,
  `wall_deadline`, `output_budget{stream, streams}` (streams `stdout`,
  `stderr`, and in a test class `output_file`), `cancelled`,
  `infrastructure{causes, causes_dropped}`. `execution`: `not_started`,
  `transferred`, `started{status, stop_trigger, enforced_at_us,
  violations, cleanup, kill_attempts}`. `Cause`: `not_started{reason}`,
  `status_unknown`, `cleanup_incomplete{survivors}`,
  `supervisor_error{error}` (`register`, `wait`, `pipe_read`, `kill`,
  `reap`, `reap_timeout`, `accounting`, `worker`),
  `capture_loss{stream, loss}` (`write_error`, `write_stalled`,
  `pipe_read`, `no_eof_before_drain_deadline`, `forward_incomplete`).
  `Status`: `exited{code, ntstatus}`, `signalled{signal, core_dumped}`,
  `unknown`. `Trigger`: `wall_deadline`, `output_budget{stream}`,
  `cancelled`. `Violation`: `wall_deadline`, `output_budget{stream,
  offset}`. `Cleanup`: `confirmed`, `incomplete{survivors}`,
  `unverified{reason}` (`namespace_unknown`, `proc_filtered`,
  `malformed`, `io`, `scan_incomplete`, `truncated`, `info_failed`,
  `unsupported`, `query_failed`, `root_not_observed`, `observer_failed`).
  `Loss`: `budget_discard`, `write_error{os_error}`, `write_stalled`,
  `no_eof_before_drain_deadline`, `pipe_read{os_error}`,
  `forward_incomplete`. `Measurement`: `kind` (`peak_rss`,
  `peak_working_set`, `peak_job_commit`, `cpu_time`,
  `process_lifetime`), `value`, `unit` (`bytes`, `microseconds`), `scope`
  (`root`, `root_and_waited_descendants`, `job`), `aggregation`
  (`max_of_processes`, `sum`, `job_peak`), `completeness` (`final`,
  `partial`), `method`, `reason` (`unsupported_platform`,
  `not_validated`, `cleanup_not_confirmed`, `accounting_failed`).
  `InstalledLimit`: `LocalLimit` + `normalised`, `inherited_soft`,
  `inherited_hard`. Notes: E §6.1 `Note` values, `truncated_stdout`,
  `truncated_stderr`, `cleanup_unverified`, `cwd_overflow`.
- `tests[]` (§4.6): `id`, `origin` (`sample`, `custom`, `stress`,
  `imported`), `answer` (`absent`, `empty`, `present`), `input_sha256`,
  `answer_sha256`, `executions` {`first`, `count`}, `class`, `check`,
  `diagnostic`, `verdict`, `score`, `validation`, `judges`, `session`,
  `output_file`. Check: `passed`, `failed{fail: wrong_answer, reason,
  diag}`, `unchecked{reason}` (`no_answer`, `checking_unverified`,
  `checking_interrupted`), `checker_failed{class: test_data_error, reason:
  invalid_answer, diag}`, `check_error{os_error}`, `not_admitted{reason}`
  (`run_not_eligible`, `cancelled`), `internal_error{invariant}`,
  `rejected{fail, role}` (`wrong_answer`, `presentation_error`),
  `scored{raw, scale, role}`, `judge_failed{class, role, reason}`,
  `output_rejected{reason}` (`output_missing`, `output_misnamed`,
  `output_not_regular`, `output_unreadable`, `output_too_large`,
  `output_dir_too_large`). `Diag` reasons: `token_mismatch`,
  `line_mismatch`, `byte_mismatch`, `count_mismatch`, `class_mismatch`,
  `integer_mismatch`, `tolerance`, `ulp_excess`, `out_of_range`,
  `incomplete_output`, `invalid_answer`. `Diag` (compare spec §6, wire
  form there; `kind` then): `token_mismatch` {`index`, `o`, `a`},
  `line_mismatch` {`line`, `o`, `a`}, `byte_mismatch` {`offset`, `o`, `a`
  (byte as a JSON number, `null` at EOF)}, `count_mismatch` {`unit`
  (`tokens`, `lines`), `o`, `a`, `first_extra`}, `class_mismatch`
  {`index`, `o`, `a`, `expected` (`int`, `dec`, `special`, `other`,
  `numeric`)}, `integer_mismatch` {`index`, `o`, `a`}, `tolerance`
  {`index`, `o`, `a`, `o_val`, `a_val`, `scale`, `diff_scaled`,
  `b_abs_scaled`, `b_rel_scaled` (each `[F, bool]` or `null`), `diff`
  (`F` or `null`)}, `ulp_excess` {`index`, `o`, `a`, `dist`, `ulps`},
  `out_of_range` {`index`, `o`}, `invalid_answer` {`index`, `a`},
  `incomplete_output`; `Item` {`offset`, `line`, `col`, `excerpt`,
  `truncated`}; `F` {`text`, `bits`}; `u64`/`u128` as decimal strings.
  Test verdicts (§2.5): `passed`,
  `wrong_answer`, `unchecked`, `checker_failed`, `wall_deadline`,
  `output_budget`, `signal`, `nonzero_exit`, `cancelled`,
  `infrastructure`, `not_run{reason}`, `internal_error{invariant}` (with
  `internal_policy_error`), `presentation_error`, `scored`,
  `session_limit`, `idle`. `judge_failed.class`: `declared_fail`,
  `protocol_unsupported`, `report_invalid`, `artefact_invalid`,
  `crashed`, `test_data_error`; `.role`: `checker`, `validator`,
  `interactor`; `.reason`: `declared_fail`, `exit_unsupported`,
  `outcome_unsupported`, `points_not_enabled`, `outside_authority`,
  `report_missing`, `report_not_regular`, `report_unreadable`,
  `report_too_large`, `report_undecodable`, `report_malformed`,
  `report_schema`, `points_invalid`, `disagreement`, `tout_missing`,
  `tout_not_regular`, `tout_unreadable`, `tout_too_large`, `signal`,
  `exit_code`, `wall_deadline`, `output_budget`, `dialogue_output`,
  `peer_allowance_expired`, `invalid_input`, `validation_failed`.
  `validation`: `execution`, `profile`, `outcome` (`valid`, `invalid`,
  `validation_failed`, `validator_failed`, `not_run`), `stderr`. `Judge`:
  `role`, `execution`, `profile`, `result` (`passed`, `rejected{fail}`,
  `scored{raw, scale}`, `judge_failed{class, reason}`, `unverified`,
  `not_read{reason}`: `stopped`, `cleanup_incomplete`,
  `extraction_failed`), `message`, `stderr`, `report` {`path`, `bytes`,
  `encoding` (`windows_1251`, `utf_8`, `null`)}. `output_file`:
  `collected{bytes, sha256, path}`, `missing`, `misnamed`,
  `too_many_entries`, `not_regular{type}` (`link`, `directory`, `other`),
  `unreadable{os_error}`, `too_large{bytes, budget}`,
  `not_collected{reason, os_error}` (`io`, `timeout`, `unstable`),
  `not_attempted`.
- `session` (§4.6): `policy` (`interactive_v7`), `checking`, `mode`,
  `verdict` (`passed`, `rejected{fail, role}`, `scored{raw, scale,
  role}`, `wall_deadline`, `output_budget`, `runtime_error`,
  `session_limit`, `idle`, `checker_failed{class, role, reason}`,
  `unchecked{reason: checking_unverified}`, `cancelled`,
  `infrastructure`, `internal_policy_error`), `decided_by`
  (`infrastructure{fact}`, `decisive{fact}`, `cancelled{fact}`,
  `authority{fact}`, `invariant{rule}`, `transferred{role}`), `facts`,
  `deadlines` {`solution_us`, `interactor_us`, `session_us`,
  `peer_allowance_us`, `idle_us`}, `directions` (`direction`: `s_to_i`,
  `i_to_s`; `read`, `forwarded`, `discarded_over_cap`,
  `delivery_incomplete`, `unread` (`none`, `unknown`), `queue_bytes`,
  `queue_records`, `end` (`eof`, `sink_gone`, `teardown`)), `transcript`
  (`complete{dir}`, `gapped{dir, gaps, dropped_records, dropped_bytes}`,
  `failed{error, at_offset}`, `unavailable`, `removed`), `seeds`
  {`solution`, `interactor`: `none`, `env{value}`, `argv{value}`},
  `early_reader_close`, `sigpipe_normalised`, `timing` {`elapsed_us`,
  `solution_cpu_us`, `interactor_cpu_us`}, `idle` {`limit_us`,
  `elapsed_us`, `read`, `forwarded`, `queued`}, `rerun` {`of_job`,
  `of_test`, `reproduced`}. `Fact` (`obs_seq`, `decision_pos`, then):
  `started{role}`, `not_started{role, reason {kind, code, text}}`,
  `checker_skipped{reason}` (`not_eligible`, `cancelled`),
  `stop_requested{role, cause}` (`own_limit{limit}`: `wall`,
  `dialogue_output`; `peer_allowance_expired`, `session_limit`, `idle`,
  `teardown{reason}`: `peer_decided`, `cancel`, `infrastructure`),
  `cancel_requested`, `root_exited{role, status, provenance}`
  (`natural`, `stopped{by}`), `cleanup_result{role, cleanup}`,
  `validation_done{role, outcome}` (`ok{result}`, `failed{class,
  reason}`, `unverified`, `not_read`), `authority_complete{result}`,
  `infrastructure{detail {kind, text}, suppressed}` (detail kind includes
  `judge_extraction`). Fact roles: `solution`, `interactor`, `checker`.
  `BText`: `text`, `truncated`.
- `aggregates[]` (§4.7): `test`, `iterations`, `succeeded`, `wall_us`
  {`min`, `median`, `max`}.
- `result` (§4.8): `verdict`, `primary_verdict`, `partial`,
  `cancellation` {`requested_at_us`, `source` (`console`, `terminate`,
  `api`), `phase` (`primary`, `fact`, `hooks`, `settlement`, `closure`)},
  `pre_delivery_exit` {`code`, `reraise` (`sigint`, `sigterm`, `null`)},
  `counts` {`primary`, `hooks` (`Cov`: `planned`, `started`,
  `not_started`, `transferred`, `skipped`, `admitted`, `invalid`),
  `tests`, `by_verdict` (one `u32` per §2.5 token, 16 members),
  `by_class` (one per §2.2 class token, `skipped`, `unknown`)},
  `internal` {`invariant`, `locus` (`job`, `build`, `execution{index}`,
  `test{test}`, `phase{phase}`), `message`}, `score` (`points{aggregation,
  value, max, full}`, `incomplete{aggregation, missing}`, `no_tests`).
  Job verdicts (§2.7, §2.14): `internal_error`, `infrastructure`,
  `cancelled`, `submit_version_mismatch`, `submit_failed`,
  `build_failed`, `wall_deadline`, `session_limit`, `idle`,
  `output_budget`, `signal`, `nonzero_exit`, `checker_failed`,
  `wrong_answer`, `presentation_error`, `unchecked`, `no_tests`,
  `scored`, `passed`, `succeeded`, `stress_cleanup_failed`,
  `candidate_failure`, `judge_failure`, `oracle_failure`,
  `validator_failure`, `generator_failure`, `no_iterations`, `refused`.
- `fact` (§4.9): `outcome` (`published`, `superseded`,
  `not_published{reason, message}`: `sequence_unavailable`,
  `epoch_changed`, `fact_too_new`, `fact_unreadable`, `lock_busy`, `io`;
  `not_attempted{reason}`: `cancelled`, `not_completed`), `warnings`
  (`durability_uncertain`, `cleanup_pending`).
- `submission` (submission contract §9.2): `mode` (`source_copy`,
  `stage`), `id`, `publication` (`published{current}`,
  `not_published{reason, message}`: `lock_busy`, `io`, `exists`,
  `current_too_new`, `current_unreadable`; `not_attempted{reason}`:
  `not_eligible`, `cancelled`), `preparation` (`selected`,
  `artifact_invalid{reason, path}`: `not_regular`, `name`, `empty`,
  `too_many`, `too_large`, `submit_file_missing`, `provided_collision`,
  `source_missing`, `manifest_too_large`; `not_reached`), `path`,
  `primary`, `files` [{`path`, `sha256`, `bytes`}; 16] + dropped,
  `files_count`, `bytes`, `inputs_sha256`, `tested`, `tool_version`
  (`pinned{line}`, `mismatch{line}`, `unpinned`), `judge`, `submit_url`,
  `clipboard` (`not_requested`, `copied`, `skipped{reason}`:
  `no_primary`, `too_large`, `not_text`, `not_published`, `cancelled`;
  `unavailable{reason}`: `no_display`, `tool_missing`;
  `failed{reason, message}`: `exit`, `timeout`, `helper`, `io`,
  `win32`), `test_check`, `external_dir` {`path`, `state` (`removed`,
  `leftover`)}.
- `hooks[]` (§4.9): `name`, `trigger`, `execution`, `outcome`
  (`succeeded`, `failed{class}`, `cancelled`, `skipped{reason}`,
  `not_planned{reason}`: `untrusted`, `disabled`, `config_override`,
  `depth`; `internal_error{invariant}`).
- `warnings[]` (§4.10), `kind` and extra members: `prune` {`job`,
  `detail`: `rename_failed`, `newer_schema`, `lease_unreadable`,
  `unmeasurable`, `measurement_incomplete`, `reclaim_leftover`,
  `reclaim_name_taken`, `test_dir_kept`, `test_dir_lost`}, `recovery`
  {`paths`, `first`}, `persist` {`artefact`: `spec`, `record`,
  `cleanup_log`; `run` (`rid` or `irid`); `error`}, `durability`
  {`artefact`: `capture_manifest`, `job_metadata`, `record`, `fact`,
  `stress_failure`;
  `detail`: `uncertain`, `cleanup_pending`}, `retention` {`detail`:
  `cwd_overflow`, `workspace_over_target`; `run` (`rid` or `irid`);
  `bytes`}, `cleanup_unverified` {`runs`}, `fact` {`outcome`}, `hook`
  {`name`, `outcome`}, `hooks_disabled` {`reason`, C §8.3},
  `late_cancellation` {`phase`}, `progress_dropped` {`lines`},
  `judge_scratch` {`run`, `detail`: `staged_copy_kept`,
  `transcript_kept`}, `testlib_header` {`role`, `detail`:
  `not_identified`, `not_pinned`, `declared`}, `tool_unpinned`,
  `clipboard` {`outcome`}, `submit_test_dir` {`detail`: `leftover`},
  `io_scratch` {`run`, `detail`: `staged_input_kept`, `output_kept`,
  `partial_copy_kept`, `not_measured`}; every warning ends with
  `message`.
- `events` {`dropped`}; `timing` {`started_at`, `finished_at`,
  `duration_us`}.
- `refusal` (§4.12): `class` (`config`, `workspace`, `cancelled`,
  `infrastructure`, `internal`), `error` {`kind`, `message`, `path`},
  `diagnostics` [`Diagnostic` {`file`, `key`, `line`, `col`, `message`};
  64] + dropped.
- `stress` (§4.14): `mode` (`search`, `replay`, `replay_binaries`),
  `shape` (`batch`, `interactive`), `seeds` {`start`, `last`}, `budget`
  {`iterations`, `time_us`}, `stop` {`reason`: `cancelled`,
  `build_failed`, `failure`, `infrastructure`, `admission_closed`,
  `cleanup_failed`, `iterations`, `time_budget`, `counter_limit`,
  `replay`; `iteration`}, `iterations` {`admitted`, `retired`, `saved`,
  `promoted`, `not_saved`}, `runs`, `captured_bytes`, `binaries_bytes`,
  `classes` [`ClassAggregate`; ≤8] (`class`: `passed`,
  `generator_failure`, `validator_failure`, `oracle_failure`,
  `judge_failure`, `candidate_failure`, `infrastructure`, `cancelled`;
  `count`, `first` {`iteration`, `seed`}, `diagnostics` [`StressDiag`
  {`iteration`, `seed`, `role`, `reason`, `source` (`message`, `stderr`,
  `compare_diag`, `none`), `text`}; 16] + dropped), `failures`
  [`FailureRef`; 20] + `failures_dropped` (`u64`) (`id`, `iteration`,
  `seed`, `class`, `role`, `reason`, `saved` (`promoted{path,
  binaries}`: `all`, `partial`, `none`; `not_promoted{reason, path,
  message}`: `lock_busy`, `exists`, `cross_filesystem`, `unsupported`,
  `problem_changed`, `path_blocked`, `io`; `not_saved{reason}`: `cap`,
  `retention_budget`, `io`, `manifest_too_large`, `resources_held`,
  `replay`), `runs` [`FailureRun` {`role`, `class`, `record`}; ≤8]),
  `timing` [`RoleTiming` {`role`, `runs`, `wall_us` {`min`, `max`,
  `total`}}; ≤8], `replay` {`of`, `original_class`, `reproduced`,
  `from_input`, `binaries`}, `cleanup_failed` {`paths` + dropped}. Run
  roles (stress contract §3.1): `generator`, `validator`, `brute`,
  `candidate`, `checker`, `brute_interactor`, `brute_checker`,
  `candidate_interactor`, `candidate_checker`; deciding role also
  `none`; reasons: stress contract §4.
- `persistence` (§4.11): `report` (`published{path}`, `failed{error}`,
  `not_attempted{reason: refusal}`), `durability` (`ok`, `uncertain`,
  `null`).

### A.4 Event envelopes (§7.3)

Header: `schema`, `document` (`event_stream`), `platform`, `command`,
`cpf_version`, `output`. Envelopes (`event`, `seq`, then): `job_started`
{`job`, `kind`, `sequence`}, `run_started` {`run`, `execution`, `stage`,
`role`, `digest`}, `output_chunk` {`run` (`rid` or `irid`), `stream`,
`bytes`}, `run_finished` {`run`, `summary`}, `progress` {`done`,
`total`}, `job_finished` {`job`, `verdict`, `report` {`path`},
`dropped`}, `iteration_started` {`iteration`, `seed`, `seeds`
{`interactor`, `solution`}}, `iteration_run_started` {`run`, `stage`
(`generator`, `brute`, `run`, `checker`, `validator`, `interactor`),
`role`, `digest`}, `iteration_run_finished` {`run`, `summary`},
`iteration_finished` {`iteration`, `class`, `role`, `reason`, `saved`};
final envelope `report` {`report`}.

### A.5 Non-job documents (§6.1, §4.12, §4.15)

- `error`: `command` (any command token, including `fetch`, `status`,
  `ls`, `search`, `notes`, `archive`, `restore`, `stress`,
  `submit_prep`), `class`, `exit`, `error`, `diagnostics` + dropped.
- `status`: `problems` [`Problem` {`problem`, `tags`, `user_status`,
  `missing_registered`, `untracked`, `targets` [`Target` {`target`
  (`solution`), `fact_state` (`missing`, `unreadable`, `malformed`,
  `too_new`, `valid`), `comparison` (`unchanged`, `source_changed`,
  `tests_changed`, `source_and_tests_changed`, `unknown`),
  `last_verdict`, `finished_at`, `capture_check`, `judge` (`none`,
  `unchanged`, `changed`, `unknown`)}], `diagnostics`, `findings`
  [`OrgFinding`], `submit` {`state` (`none`, `current`, `stale`,
  `unknown`), `id`, `mode`, `tested`}, `fetch` (`FetchState` {`record`
  (`missing`, `unreadable`, `malformed`, `too_new`, `valid`),
  `attempted_at`, `outcome` (`committed`, `unchanged`, `conflict`,
  `refused`, `failed`, `rolled_back`), `txn`, `recovered`, `incomplete`
  (`pending`, `committed`, `finished`, `undetermined`)})}].
- Organisation types: `OrgFinding` {`field` (`tags`, `status`,
  `created_at`, `updated_at`, `solved_at`, `archived_at`), `reason`};
  `Source` {`path`, `kind`, `field`, `message`} with `kind` `link`,
  `not_a_directory`, `invalid_name`, `name_collision`,
  `metadata_missing`, `metadata_unreadable`, `metadata_malformed`,
  `metadata_too_new`, `identity_mismatch`, `contest_unreadable`,
  `contest_malformed`, `contest_too_new`, `invalid_field`,
  `notes_not_regular`, `notes_unreadable`, `notes_not_utf8`,
  `root_unavailable`, `not_an_archive_root`, `root_record_unreadable`,
  `root_record_malformed`, `root_record_too_new`, `roots_unavailable`,
  `unreadable_directory`, `summary_malformed`, `summary_too_new`,
  `summary_too_large`, `manifest_missing`; `origin` (`active`,
  `snapshot`); `Snapshot` {`id`, `kind` (`problem`, `contest`), `root`,
  `path`, `complete`}.
- `ls`: `kind` (`problems`, `contests`, `archived`, `archived_contests`),
  `problems` [`LsProblem` {`problem`, `origin`, `title`, `tags`,
  `user_status`, `effective_status`, `created_at`, `updated_at`,
  `solved_at`, `archived_at`, `contests`, `findings`, `snapshot`}],
  `contests` [`LsContest` {`contest`, `identity`, `tags`, `members`,
  `counts` [{`status`, `count`}], `unresolved`, `invalid`, `findings`,
  `snapshot`}], `invalid` [`Source`].
- `search`: `terms`, `notes`, `archives`, `hits` [`Hit` {`problem`,
  `origin`, `fields` (`identity`, `title`, `url`, `tags`, `status`,
  `notes`), `notes_lines` + dropped, `notes_truncated`, `snapshot`}],
  `findings` [`Source`], `incomplete`, `incomplete_reasons` (S §5.9.8),
  `entries`, `bytes_read`.
- `notes`: `problem`, `path`, `state` (`created`, `existing`, `absent`,
  `not_regular`).
- `stress_failures`: `problem`, `entries` [`Entry` {`name`, `state`
  (`valid`, `unexpected`, `missing`, `unreadable`, `malformed`,
  `too_new`), `detail`, `id`, `class`, `role`, `reason`, `iteration`,
  `seed`, `shape`, `created_at`, `binaries` (`all`, `partial`, `none`)};
  10 000] + dropped.
- `submit`: `problem`, `submission`, `warnings` + dropped.
- `archive_plan`: `plan`, `target` (`ArchiveTarget` {`kind` (`problem`,
  `contest`), `identity`, `dir`}), `problems` [`PlanProblem` {`problem`,
  `dir`, `decision` (`retire`, `export`, `snapshot_only`), `reason`,
  `destination` {`root`, `path`}, `cleanup`, `classes` [{`class`,
  `files`, `bytes`, `kept`}], `other_files`, `omissions` [{`what`,
  `count`, `bytes`}], `externals` [{`kind`, `value`, `key`}],
  `selection`, `contests`, `delete_warning`}], `warnings`
  (`network_root`, `root_not_in_roots`); token values of `reason`,
  `cleanup`, `class`, `what` and `kind`: archive contract §§3.1–3.7.
- `archive`: `plan`, `problems` [`ArchiveOutcome`], `contest`
  (`ContestOutcome` {`contest`, `snapshot`, `complete`, `retired`}),
  `resumed` [`ArchiveOutcome`]; `ArchiveOutcome` {`problem`, `snapshot`,
  `reused`, `retirement` (`retired`, `exported`, `snapshot_only`,
  `refused`), `reason`, `txn`, `selection_cleared`, `cleanup`
  (`ArchiveCleanup` {`state` (`complete`, `incomplete`), `trashed`,
  `quarantined`, `deleted`, `snapshot_only`, `quarantine`, `unsettled`
  [{`path`, `reason`}; 64] + dropped})}.
- `restore`: `snapshot`, `restored` [{`problem`, `dir`, `origin`,
  `history` (`restored`, `not_restored`, `pending`)}], `contest`,
  `omissions` [{`what`, `count`, `bytes`}], `externals` [{`kind`,
  `value`, `key`, `exists`}], `findings` (archive contract §7.3).
- `fetch`: `selection` (`FetchSelection` {`kind` (`problem`, `contest`,
  `problem_set`), `identity`, `url`, `members`}), `problems`
  [`FetchProblem` {`identity`, `outcome` (`committed`, `unchanged`,
  `conflict`, `refused`, `failed`, `rolled_back`), `failure`
  (`FetchFailure` {`class` (sites §8.3: `auth_required`, `challenge`,
  `rate_limited`, `not_found`, `not_available`, `unsupported`,
  `unrecognized`, `parser_failure`, `network`, `offline_miss`),
  `reason`, `http_status`, `retry_at`}), `provenance` {`url`,
  `final_url`, `fetched_at`, `status`, `body_sha256`, `from_cache`},
  `warnings`, `findings` (`FetchNote` {`kind`, `ordinal`, `test`,
  `detail`}; warning kinds `no_samples`, `tolerance_hint`,
  `multiple_answers_hint`, `language_fallback`, `limits_unparsed`,
  `title_unparsed`, `interactive_samples_skipped`, `zero_samples`,
  `edited_sample_kept`, `status_not_written`, `durability_uncertain`,
  `cleanup_pending`; finding kinds `duplicate_ordinal`,
  `duplicate_recorded`, `duplicate_upstream`, `moved`, `added`,
  `updated`, `kept_edited`, `removed_upstream`, `removed_locally`,
  `name_taken`, `base_changed`, `edited_during_commit`,
  `metadata_conflict`, `metadata_not_applied`, `metadata_pending`,
  `history_pending`, `history_blocked`, `journal_pending`,
  `abort_unrecorded`), `txn`}] (outcome also `cancelled`), `network`
  {`wire_requests`, `connection_attempts`, `redirects`, `retries`,
  `revalidated`, `offline`, `elapsed_ms`, `budget_requests`,
  `budget_seconds`}, `cancelled`.
- `config_show`: C §10.

### A.6 Exit statuses

- Codes (§5.1): 0 `success`, 1 `failed`, 2 `usage`, 3 `build_failed`, 4
  `not_verified`, 5 `checker_failed`, 6 `cancelled`, 7 `config`, 8
  `workspace`, 9 `partial`, 10 `infrastructure`, 11 `delivery`, 12
  `site`, 70 `internal`; 13–63 reserved; the Unix re-raise of `SIGINT`
  and `SIGTERM` (§5.3).
- Failure classes (§1): `usage`, `config`, `workspace`, `partial`,
  `cancelled`, `internal`, `site`.
- Precedence rows (§5.2), in order: 1 usage → 2; 2 forced cancellation →
  §5.3; 3 internal → 70; 4 delivery → 11; 5 orderly cancellation → §5.3;
  6 config, `submit_version_mismatch` → 7; 7 workspace, submission
  `not_published` → 8; 8 partial, submission `published{current: false}`
  → 9; 9 site → 12; 10 `infrastructure`, `stress_cleanup_failed` → 10;
  11 `build_failed`, `submit_failed` → 3; 12 `checker_failed`,
  `judge_failure`, `oracle_failure`, `validator_failure`,
  `generator_failure` → 5; 13 `wall_deadline`, `output_budget`,
  `signal`, `nonzero_exit`, `wrong_answer`, `session_limit`, `idle`,
  `presentation_error`, `candidate_failure` → 1; 14 `unchecked`,
  `no_tests`, `no_iterations` → 4; 15 `scored` not full → 1; 16 → 0.
  The several-problems rule and the `interactor-test` expectation rule
  (§5.2), the `cpf switch` rules (§2.10, §5.2).
- Per-command rows (§5.5), 37 rows, each with its status set:

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
| `cpf test add` | 0, 8, 9; also with `--from-stress` |
| `cpf stress` (search, `--replay`, `--replay-binaries`) | 0, 1, 3, 4, 5, 7, 8, 10, 11, cancellation |
| `cpf stress ls` | 0 (any entries, any classification), 8, 11 (`--json`) |
| `cpf test register`, `enable`, `disable` | 0, 8 |
| `cpf test ls` | 0; 8 when the catalog is not `missing` or `valid` (listing still printed) |
| `cpf config show` | 0, 7, 11 (`--json`, C §10) |
| `cpf config check` | 0, 7 (no `--json` in schema 1) |
| `cpf config paths` | 0 |
| `cpf doctor` | 0, 7; an unsupported capability is never an error |
| `cpf status` | 0, 8, 11 (`--json`); filters add only 2 for a filter value outside its grammar |
| `cpf clean` | 0, 8; `--apply`: 9 when a selected candidate could not be renamed or deleted (held leases are skipped) |
| `cpf submit-prep`, source copy | 0, 7, 8, 9, 11 (`--json`) |
| `cpf submit-prep`, stage mode | 0, 1, 3, 4, 5, 7, 8, 9, 10, 11, cancellation |
| `cpf fsck` | 0, 8 (findings are reported with 0) |
| `cpf adopt`, `ignore`, `gitignore` | 0, 8 |
| `cpf tag`, `cpf untag` | 0, 8 |
| `cpf mark` | 0, 7, 8 |
| `cpf notes` | 0, 8, 11 (`--json`); `--path`: 0, 8, 11 |
| `cpf ls` | 0 (also with invalid entries and findings), 8, 11 (`--json`) |
| `cpf search` | 0 (hits or none, complete or incomplete), 8, 11 (`--json`), cancellation |
| `cpf archive` | plan: 0, 7, 8, 11 (`--json`); `--apply`: 0, 7, 8, 9, 11; `--resume`: 0, 8, 9, 11 (`--json`); `--close`: 0, 8, 9 (archive contract §5.4) |
| `cpf restore` | 0, 8, 9, 11 (`--json`); 7 when a snapshot id is looked up and the configuration that holds `archive.roots` is invalid (a snapshot path loads no configuration) |
| `cpf quarantine ls` | 0, 8 |
| `cpf quarantine purge` | 0, 8; `--apply`: 9 when a selected entry could not be renamed or deleted |
| `cpf ls --archived` | as `cpf ls`, plus cancellation |
| `cpf fetch` | 0, 7, 8, 9, 11 (`--json`), 12, cancellation |
| `cpf fetch --history` | 0, 8 |
| `cpf interactor-test` | 0, 1, 3, 4, 5, 7, 8, 10, cancellation |
| `cpf --help`, `--version` | 0 |
