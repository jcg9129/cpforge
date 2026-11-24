# Stress contract

Status: v1. Normative for `cpf stress` (search, `--replay`,
`--replay-binaries`), `cpf stress ls`, `cpf test add --from-stress`, the job
engine's iteration lifecycle and saved failures. It is the stress amendment
of the other contracts: the configuration table `[stress]` (configuration
contract §9.2), the iteration lifecycle rules of the execution contract
(§§1, 2, 3.3, 5.4, 6.2, 9), the stress report, events, exit rows and human
lines (report contract §§2.14, 3.7, 4.14, 4.15, 5, 7.3, 8, 9.7) and the
saved-failure tree (storage contract §§3, 5.3, 5.5, 5.6, 6.1, 9, 10).

References labelled *E*, *S*, *C* and *report contract* point to
`execution.md`, `storage.md`, `config.md` and `report.md`; *design* to
`../design.md` (§6.5 Stress); *checkers design* to `../design-checkers.md`
(§7 seeds and reruns, §8 interactive stress). Judging (checkers,
validators, interactors, their profiles, private directories, staging,
extraction and the session reducer) is reused unchanged from the judging
amendment (E §2.1, C §9.1, report contract §§2.11–2.13).

MUST / MUST NOT / never are requirements. Defaults are overridable by
configuration unless marked *fixed*. The default budgets (10 000
iterations, 60 s, a random printed start seed), the save caps (20 per job,
256 MiB of preserved programs, 64 MiB per program), the stop and no-save
rule for infrastructure and cancelled iterations, the gitignore comment
and the refusal of file-based I/O are settled (decisions of 2026-11-24).

**Guarantee, stated once.** A seeded rerun is not a determinism promise
(design §6.5, checkers design §7): a program that reads the clock, its
address space, uninitialised memory or thread timing can fail once and pass
on replay. cpf reports whether a replay reproduced the saved class; it
never claims that a passing replay proves a fix, and it never claims timing
equivalence.

## 1. Terms

- **Stress job**: a job of kind `stress` (report contract §1). It builds
  every role once, then runs **iterations** until a stop condition (§5)
  holds. Its plan (C §8.2) is its builds only; iterations are not planned
  executions.
- **Iteration** *k* (a `u64`, from 0): one generated input and every run
  that judges it. Its **generator seed** is `g_k = start + k` (§3.2).
- **Iteration run**: one supervised run of an iteration (E §1 run), with
  the **iteration run id** `<k>.<role>` (§3.1 role tokens), e.g.
  `17.candidate`. A batch iteration has at most 5 runs, an interactive one
  at most 8 (§3.3, §3.4). Iteration runs never get a report run id `r…`
  (report contract §3.4); builds do.
- **Live iteration**: the iteration whose first run has been admitted and
  which is not yet **retired** (§6.3). A stress job has at most one.
- **Outcome class**: the classification of a retired iteration (§4):
  `passed`, `generator_failure`, `validator_failure`, `oracle_failure`,
  `judge_failure`, `candidate_failure`, `infrastructure`, `cancelled`.
  The five `…_failure` classes are **failing classes**.
- **Saved failure**: a failing iteration published as a directory
  `problems/<p>/stress/failures/<id>/` (§7), user-owned once published.
- **Cumulative counters**: iterations admitted and retired, runs admitted
  and started, captured bytes, per-class counts, saved and counted-only
  failures. Each is a `u64` with checked arithmetic and no other bound
  (§6.6).
- **Simultaneously held resources**: admission slots and thread credits
  (E §3.3), byte reservations and retained bytes (E §5.4), run directories,
  captures, staged judge files, transcripts, in-memory records and
  persistence artefacts. Their bound is the live iteration's plus a fixed
  part (§6.6), whatever the number of iterations.

## 2. Commands

### 2.1 `cpf stress` (search)

```
cpf stress [--problem <p>] [--profile <name>] [--deadline <d>]
           [--seed <u64>] [--iterations <u64>] [--time <duration>]
           [--keep-going] [--json | --events[=output]]
```

- Needs a workspace and a selected problem (S §5.7) whose effective kind
  (C §9) is `batch` or `interactive` and whose `problem.toml` has a valid
  `[stress]` table (C §9.2). The candidate is the selected solution target
  (C §6.2), with `--profile` and `--deadline` as in `cpf test` (C §6.1
  row 6, §7.1).
- `--seed S`: the start seed, a decimal `u64` in the C §9.1.6 seed grammar.
  Absent: the start seed is drawn from the operating system's random source,
  uniformly in [0, 2⁶³ − 1], recorded in the report (`stress.seeds.start`)
  and printed, so the same run can be repeated with `--seed`.
- `--iterations N`: the iteration budget, a decimal `u64`, N ≥ 1; default
  `[stress] iterations` (C §9.2: 10 000).
- `--time D`: the time budget, a C §7.1 duration (1 ms to 24 h); default
  `[stress] time_s` (60 s).
- **Wrapping is refused**: start + N − 1 > `u64::MAX` is a phase-3 plan
  error (exit 7) naming both values, before admission. Seeds never wrap.
- `--keep-going`: continue after a failing iteration (§5); without it the
  first failing iteration stops the job.
- `--json` and `--events` as for `cpf test` (report contract §§6, 7).
- Usage errors (exit 2): a value outside its grammar, `--iterations 0`,
  `--json` with `--events`, and any search option (`--seed`,
  `--iterations`, `--time`, `--keep-going`) together with `--replay` or
  `--replay-binaries`.

### 2.2 `cpf stress --replay <id>` and `--replay-binaries <id>`

```
cpf stress --replay <id>          [--problem <p>] [--profile <name>] [--deadline <d>] [--json | --events[=output]]
cpf stress --replay-binaries <id> [--problem <p>] [--json | --events[=output]]
```

`<id>` is a saved-failure id (§7.1) of the selected problem; an argument
outside the grammar is a usage error (2). Both options are mutually
exclusive. The job has kind `stress` and mode `replay` or
`replay_binaries` (§8).

### 2.3 `cpf stress ls`

```
cpf stress ls [--problem <p>] [--json]
```

A lock-free reader (S §8 reader rules) of `problems/<p>/stress/failures/`:
one line per entry in byte order of the entry name, with its
classification (§7.6), and for a `valid` entry the id, class, reason,
deciding role, iteration, seed, shape, creation time and whether programs
were preserved. It never verifies file hashes (§7.6: hashes are verified
at use), and says so. An absent `stress/failures/` directory lists
nothing. `--json` writes the document `stress_failures` (report contract
§4.15). Exit: 0 (with any number of entries in any classification), 8
(no workspace, selection error, the problem record not `valid`), 11
(`--json` delivery). Works on every OS: it starts no process.

### 2.4 `cpf test add --from-stress <id>`

Registers a saved failure's input as a test of the selected problem,
by the S §5.3 `test add` transaction as amended (S §5.3): the input, and the answer iff the failure records one
(`answer` key of §7.3, present only when the brute answer rule held,
§3.5), are read from the failure directory before the lock, verified
against `failure.toml` while being captured, and published as
`tests/stress-<n>.in` / `.ans` with `origin = "stress"`. A failure that
is not `valid`, or a file whose length or SHA-256 differs from the
manifest, refuses before the lock with nothing published (exit 8). An
interactive failure registers its input without an answer. Exit 0, 8, 9
(report contract §5.5). Works on every OS.

### 2.5 Platform

`cpf stress` in every mode starts processes. On Windows it is refused
with exit 10 before admission and before any mutation, at the existing
validation boundary (report contract §5.2, runtime decision record §3):
configuration phases 1–2 (C §3; `[check]` and `[stress]` included) and the
early refusals are checked first and give exit 7; the platform refusal
follows; saved-failure classification (§2.2) and phase-3 planning are not
reached. `cpf stress ls` and `cpf test add --from-stress` work on every OS.
The lifecycle of §6 is tested on all three OSes through the job engine with
an injected executor (§10.1).

## 3. Roles and the iteration plan

### 3.1 Roles, builds and run roles

| Role | Declared by | Runs per iteration |
|---|---|---|
| candidate | the selected solution target (C §6.2) | batch: one run; interactive: one session with an interactor |
| generator | `[stress] generator` (C §9.2), required | one run, always first |
| brute | `[stress] brute` (C §9.2) | batch: one run; interactive: one session with an interactor |
| validator | `[check] validator` (C §9.1) | one run, if declared |
| checker | `[check] checker` (kind `checker`; interactive checking `checker` or `interactor+checker`) | batch: one run; interactive: one follow-up run per session |
| interactor | `[check.interactive] interactor` | interactive: one per session |

- **Builds**: once per job, before the first iteration, in the order
  candidate, checker, validator, interactor, generator, brute (at most 6
  planned executions, each with a report run id), with the build rules of
  report contract §2.3 as amended for role builds: a build that is not
  `succeeded` makes every later build `skipped{build_not_succeeded}` and
  no iteration is admitted. Generator and brute builds are role builds
  (C §9.2 role specs, C §9.1.2 stage composition, C §9.1.3 capture into
  `inputs/roles/generator/` and `inputs/roles/brute/`); their capture
  check follows C §9.1.3. They are not judges: the judge fingerprint
  (C §9.1.3) does not cover them; each saved failure records their
  captured-content digests (§7.3 `[[build]]`).
- **Run roles** (the `<role>` of an iteration run id, *fixed* tokens):
  batch `generator`, `validator`, `brute`, `candidate`, `checker`;
  interactive `generator`, `validator`, `brute`, `brute_interactor`,
  `brute_checker`, `candidate`, `candidate_interactor`,
  `candidate_checker`. The longest token has 20 bytes.
- **Stages** (E §2): the generator run has stage `Generator`, the brute
  run (batch) or the brute's solution run (interactive) `Brute`, the
  candidate's `Run`, judges their judging stages (E §2.1).
- **Run templates**: every run stage of every role is composed and
  validated once, in phase 3 (C §3; the same checks as a judged plan:
  program resolution, local limits, placeholders). An iteration
  instantiates a template by substituting only its run directory, its
  stdin path, the generator seed argument and the seed environment
  variables. No configuration is read after the plan (E §2).
- Every run of an iteration has the iteration run directory
  `iterations/<k>.<role>/` of the job (S §6.1) with the E/S run layout
  (`spec.json`, `record.json`, `cleanup.log`, `out/`, `cwd/`, and for judges
  `judge/`, for relay sessions `transcript/`); its `cwd/` is fresh (S §7).

### 3.2 Seeds

- **Generator**: always invoked as `gen <seed>` (checkers design §7): the
  decimal generator seed `g_k` is appended as the last argument of the
  generator's run `argv` (to shell `args` for a shell stage, C §5.3),
  after expansion, never expanded or rebased. `g_k = start + k`; §2.1
  refuses a plan whose seeds would wrap, so `g_k ≤ u64::MAX` for every
  admissible k.
- **Interactor and solution seeds** (interactive problems, delivered only
  as declared by C §9.1.6): derived from the generator seed, so both
  sessions of an iteration get the same interactor seed (checkers design
  §8) and a search run is repeatable from its start seed:
  `seed(role, g)` = the first 8 bytes, read as a little-endian `u64`, of
  SHA-256 over the bytes `"cpf-stress-seed-v1"`, `0x00`, the ASCII token
  `interactor` or `solution`, `0x00`, then `g` as a little-endian `u64`.
  The same solution seed is given to the brute and the candidate. Vectors:
  `seed(interactor, 0)` = 1070811988136109722, `seed(solution, 0)` =
  1733034929431036035, `seed(interactor, 18446744073709551615)` =
  9348864240263994029, `seed(solution, 1)` = 2190445126481607525.
- Every delivered seed is recorded (failure manifest §7.3, report failure
  refs, `iteration_started` event). An undeclared role seed is not
  delivered and recorded as `none` (C §9.1.6).
- A replay delivers the seeds recorded in the failure manifest (§8), not
  newly derived ones.

### 3.3 Batch iteration

Runs, in this order, each admitted only after the previous one's record is
final and only when the condition in the last column holds (otherwise the
iteration is decided by §4.1 and the remaining runs are not admitted):

| # | Run role | Stdin | Stdout | Admitted when |
|---|---|---|---|---|
| 1 | `generator` | null | `out/stdout`, cap = the generator's `stdout_cap` (default 64 MiB), `Kill` (*fixed* policy) | always |
| 2 | `validator` (if declared) | per its profile (E §2.1), from the staged `input` | its own capture | the generator is `exited_zero` with a complete stdout capture |
| 3 | `brute` (if declared) | the input file | `out/stdout`, cap = the brute's `stdout_cap` (default 64 MiB), `Kill` (*fixed* policy) | the input is valid (validator `exited_zero` / valid, or none declared) |
| 4 | `candidate` | the input file | the target's run stdout (E §5.5) | the input is valid and the brute answer rule held (§3.5); without a brute, the input is valid |
| 5 | `checker` (kind `checker`) | per E §2.1 | its own capture | the candidate's class is `exited_zero` |

- The **input** of the iteration is the generator's frozen stdout capture
  file `iterations/<k>.generator/out/stdout`; the validator, brute and
  candidate read it (the brute and candidate as `StdinSource::File`), and
  judges get a staged copy (E §2.1). It is tool-created, reserved as a
  stream capture (E §5.4) and never modified after capture freeze.
- **Answer**: with a brute, the brute's frozen stdout capture
  (`iterations/<k>.brute/out/stdout`) once the brute answer rule held;
  without a brute (allowed only with `[check] kind = "checker"` and
  `answer = "none"`, C §9.2), an empty file as for `answer = "none"`
  (C §9.1.4).
- **Judging**: kind `compare` compares the candidate's stdout with the
  answer in-process (compare spec §1.1; the answer is validated first,
  compare spec §5.5); kind `checker` runs the checker with `input`,
  `output` (the candidate's stdout) and `answer` staged per E §2.1, read
  by its profile, with the extraction and judge-scratch rules of E §2.1.
- No diagnostic judging: a judge is never admitted after a candidate class
  other than `exited_zero`.
- File-based I/O (`[config] io`, C §6.2) is not supported by `cpf stress`
  in this version: a problem declaring it is a phase-2 error for `cpf
  stress` ("stress with file-based I/O is unsupported in this version").

### 3.4 Interactive iteration

Each solution (brute, candidate) runs in its own session (checkers design
§8) with the same input and the same interactor seed; transcripts are
never compared. Runs, in this order:

| # | Run roles | Admitted when |
|---|---|---|
| 1 | `generator` | always (as §3.3 row 1) |
| 2 | `validator` (if declared) | as §3.3 row 2 |
| 3 | session `brute` + `brute_interactor` (one pair, E §9a) | the input is valid |
| 4 | `brute_checker` (checking `checker` or `interactor+checker`) | eligible by checkers design §6.1 item 6 |
| 5 | session `candidate` + `candidate_interactor` | the brute session's verdict satisfies the **reference success predicate** (checkers design §8): OK, or `scored` with raw equal to the scale |
| 6 | `candidate_checker` (as row 4) | as row 4, for the candidate session |

- Each session is built as for `cpf test` (E §2.1 Sessions, C §9.1.6:
  mode, allowances, idle, caps) from the solution's run stage (the
  candidate's target stage, or the brute's role run stage), with the
  session's interactor seed and solution seed (§3.2). Its closed record is
  reduced by report contract §2.12 (policy `interactive_v7`).
- The interactor's `answer` is an empty file: interactive stress requires
  `[check] answer = "none"` (C §9.2), since a generator writes no answer.
- A brute is required for interactive stress (C §9.2).

### 3.5 Brute answer rule

The brute's output becomes the iteration's answer (design §6.5) iff all
hold, in this order, the first failing condition giving the §4.1 reason:

1. the input was valid (row 3 was admitted);
2. the brute's class (report contract §2.2) is `exited_zero` (else
   `oracle_failure` with the class token as reason);
3. its stdout capture is complete (E §5.3; a `Kill` breach is already the
   class `output_budget`, an infrastructure loss is §4.1 row S1);
4. kind `compare` only: the comparator accepts it as an answer (compare
   spec §5.5, checked in-process before the candidate is admitted; else
   `oracle_failure{invalid_answer}`). With kind `checker`
   the checker decides validity instead of comparing outputs (design
   §6.5); a checker that rejects the brute's answer is the checker's
   failure or verdict, never silently accepted.

An answer that fails the rule is never staged, compared or saved as an
answer; the brute's stdout is still saved as evidence (§7.2 `out/brute`).
Interactive stress has no answer; the reference success predicate (§3.4)
takes the rule's place.

## 4. Outcome classes

Derived once per iteration, after the last of its records is final, under
the job's verdict lock (E §6.4), as a pure function of the iteration's
records, its closed session records and the cancellation intent. The first
matching row decides. Every iteration gets exactly one class, one
**deciding role** (a run role token, or `none` for `passed` and for
cancellation before any run) and one **reason** token.

### 4.1 Batch

| # | Condition | Class | Deciding role, reason |
|---|---|---|---|
| S1 | any run of the iteration has class `infrastructure` (incl. `not_started` other than `cancelled_before_start`), or a judge's check is `check_error` (report contract §2.11 B23, comparator I/O failure §2.4) | `infrastructure` | that run; `retention_budget` for `not_started{retention_budget}`, else the primary cause's variant (report contract §2.2) or `check_error` |
| S2 | any run has class `cancelled`, coverage `transferred` or `not_started{cancelled_before_start}`, a run or check was not admitted because intent was set (E §6.4), or intent was set before the iteration's first admission | `cancelled` | that run, or `none`; `cancelled` |
| S3 | the generator's class is not `exited_zero` | `generator_failure` | `generator`; the class token (`wall_deadline`, `output_budget`, `signal`, `nonzero_exit`) |
| S4 | the validator did not report the input valid (report contract §2.11 B5–B7) | `validator_failure` | `validator`; `validation_failed` (B5), `invalid_input` (B6), the B7 class reason |
| S5 | the brute answer rule failed (§3.5) | `oracle_failure` | `brute`; the class token, or `invalid_answer` |
| S6 | the checker's check is `judge_failed{…}` (B20, B21, B24–B32, B36) or `unchecked{checking_unverified}` (B22) | `judge_failure` | `checker`; the B-row reason (`exit_code`, `report_invalid` reasons, `declared_fail`, …) or `checking_unverified` |
| S7 | the candidate's class is `wall_deadline`, `output_budget`, `signal` or `nonzero_exit`; or the check is `rejected{wrong_answer \| presentation_error}` (B34, B35), a comparator mismatch (§2.4 `failed`), or `scored` with raw < `min_score` (C §9.2) | `candidate_failure` | `candidate` (class) or `checker` (check), or `candidate` for a comparator mismatch; the class token, `wrong_answer`, `presentation_error`, the comparator's `Diag` reason, or `score_below_threshold` |
| S8 | otherwise: the candidate `exited_zero` and the check `passed`, a comparator match with a complete capture, or `scored` with raw ≥ `min_score` | `passed` | `none`; `passed` |

- A comparator match on an incomplete candidate capture without a class
  row cannot arise (report contract §2.2); if it does, it is an orphan:
  the iteration's class is `infrastructure` with reason `invariant` and an
  internal failure is recorded (report contract §2; the job verdict is
  `internal_error`, exit 70). So is any combination the table does not
  list.
- A judge failure is never a candidate failure, and an oracle failure is
  never reported as a bug in the solution (checkers design §8).

### 4.2 Interactive

Checkers design §8, as rows (the first match):

| # | Condition | Class | Deciding role, reason |
|---|---|---|---|
| T1 | a run of the generator or validator has class `infrastructure`, or either session's verdict is `infrastructure` or `internal_policy_error` | `infrastructure` | that run or session's solution role; the cause, or `internal_policy_error` (also an internal failure, report contract §2.12: job verdict `internal_error`, exit 70) |
| T2 | as S2, or either session's verdict is `cancelled` (including a cancellation snapshot, report contract §2.12 I3) | `cancelled` | as S2 |
| T3 | as S3, S4 (before any session) | `generator_failure`, `validator_failure` | as S3, S4 |
| T4 | either session's verdict is `checker_failed{…}` or `unchecked{checking_unverified}` | `judge_failure` | the session's judging role (`brute_interactor`, `brute_checker`, `candidate_interactor`, `candidate_checker`); the `judge_failed` reason or `checking_unverified` |
| T5 | the brute session fails the reference success predicate (any other brute-side verdict) | `oracle_failure` | `brute`; the session verdict token (`wrong_answer`, `presentation_error`, `wall_deadline`, `output_budget`, `runtime_error`, `session_limit`, `idle`, `score_below_scale`) |
| T6 | the candidate session's verdict is WA, PE, TLE, OLE, RE, `session_limit`, `idle`, or `scored` with raw < `min_score` | `candidate_failure` | `candidate`; the session verdict token or `score_below_threshold` |
| T7 | otherwise (the candidate session OK, or `scored` with raw ≥ `min_score`) | `passed` | `none`; `passed` |

Since the candidate session is admitted only after the brute session
satisfied the predicate (§3.4 row 5), T4 and T5 are decided on the brute
session alone when the candidate session did not run. Every terminal
verdict maps to exactly one class (§10.6 ST-I01).

## 5. Budgets and stop policy

Before each iteration admission, the job checks, in this order, under the
verdict lock; the first condition that holds sets the **stop reason** and
no further iteration is admitted. Rows 4–6 apply to search jobs only;
in a replay job (§8) row 7 replaces them. k is the iteration number of
the admission (a search's k counts from 0; a replay's is the saved one)
and a is the number of iterations the job has admitted so far:

| # | Condition | Stop reason |
|---|---|---|
| 1 | cancellation intent is set | `cancelled` |
| 2 | a build did not succeed (checked at a = 0, before any iteration, in search and replay jobs alike) | `build_failed` |
| 3 | an earlier iteration set a stop: a failing class without `--keep-going` (search jobs only: in a replay a failing class sets no stop, so row 7 decides); class `infrastructure` (also with `--keep-going`); class `cancelled`; a cleanup failure (§6.4) | `failure`, `infrastructure` (`admission_closed` when its reason is `retention_budget`), `cancelled`, `cleanup_failed` |
| 4 | a = N (the iteration budget) | `iterations` |
| 5 | the time budget has expired: now ≥ t₀ + T, with t₀ the monotonic instant the check for k = 0 began | `time_budget` |
| 6 | a cumulative counter (§1) would overflow by admitting k | `counter_limit` |
| 7 | replay: a = 1 (its one iteration has been admitted) | `replay` |

- The time budget never stops a live iteration: it decides admission
  only. A job therefore reaches report phase V by t₀ + T plus one
  iteration (its runs are bounded by their own deadlines, E §6.3) plus
  its retirement (§6.3).
- `--keep-going` continues after failing classes; saving is capped (§7.4)
  and further failures are counted only. Infrastructure always stops: an
  unreliable observation is not repeated thousands of times.
- The stop reason and the iteration at which it was decided are reported
  (report contract §4.14 `stop`).

## 6. Engine lifecycle: unbounded iterations, bounded resources

This section is the execution contract's iteration lifecycle (E §§3.3,
5.4, 6.2 refer here). The tests of §10.1 are its acceptance.

### 6.1 Iterations are not runs

- The C §8.2 cap (10 000 planned executions) applies to the job's plan,
  which for a stress job is its builds (≤ 6). Iterations are bounded by
  the iteration and time budgets (§5), never by the cap; an iteration
  is one admission unit made of ≤ 5 (batch) or ≤ 8 (interactive) process
  runs.
- **Streaming admission**: the engine never materialises a list of
  iterations or iteration runs. Iteration k is instantiated from the run
  templates (§3.1) when it is admitted and dropped at its retirement.
- Report run ids, `executions[]` entries, `run_started`/`run_finished`
  events and the `progress` event (report contract §§3.4, 4.5, 7.3) are
  kept for builds. Iteration runs are identified by `<k>.<role>` in the
  iteration events, failure refs and saved failures; they never consume a
  report run id and are never listed run by run.

### 6.2 Admission

- **One live iteration**: the first run of iteration k + 1 is not admitted
  before iteration k is retired (§6.3, step R11).
- Inside an iteration, runs are admitted one at a time in §3.3 / §3.4
  order (a session pair atomically, E §9a), each by E §3.3 (one cleanup
  slot and its thread credits, released at resources released) and E §5.4
  (its stream and metadata reservations, judge staging and transcript
  reservations). Two bounds are distinct. **Executing**: a run is created
  only after the previous run's record is final, so at most 2 iteration
  runs execute at once (a session pair). **Holding**: a run whose record
  is final keeps its slot and credits until resources released (E §3.3),
  so up to all ≤ 8 runs of the live iteration may hold slots at once (a
  stalled stderr writer, a slow reap); the process-wide 4 slots cap them,
  and a refused admission is the run's `not_started` record as for any run
  (`cleanup_capacity`, `worker`, `retention_budget`), so §4 row S1/T1
  decides and the job stops. No run of the next iteration is admitted
  while any run of the live iteration holds a slot (R6).
- **No spec at admission.** Unlike E §6.2, an iteration run's spec is not
  enqueued for persistence at admission: it is encoded at admission (the
  256 KiB check of E §2 applies, `NotStarted{SpecTooLarge}`), its digest
  computed, and the encoded bytes are held with the live iteration until
  retirement (step R5). A passing iteration persists nothing.
- **Metadata reservation ownership.** Each iteration run's 1 MiB metadata
  reservation (E §5.4) is owned by the live iteration from the run's
  admission until R5, not by the persistence thread: at R5 a `save` or
  `keep` iteration hands each run's reservation to its enqueued artefacts
  (then E §6.2 settlement owns it, late completion included), and a
  `discard` or `count` iteration releases it unused. R6's "resources
  released" is therefore the run's own resources (slot, credits, workers,
  stream reservations, staging) without the metadata reservation.

### 6.3 Retirement

Iteration k is retired by these steps, in order. Each step's bound is
stated; none waits for an unbounded event.

| Step | Action | Bound |
|---|---|---|
| R1 | every admitted run's record is finalized (E §6.2), and every session has a closed record or a **cancellation snapshot** (a pair or follow-up checker whose creation returned `Transferred`, E §§2.1, 3.4; report contract §2.12 I3), which is final without reduction and without waiting for the creation to complete | the runs' own deadlines (E §§5.5, 6.3); a snapshot is immediate |
| R2 | derive the class, deciding role and reason (§4), under the verdict lock | no I/O |
| R3 | **fold** into the aggregates (report contract §4.14): the class counter; first iteration and seed of the class; the first 16 diagnostics of the class (§9); per-role timing (count, min, max, total wall); for one of the first 20 failing iterations its failure ref (its `saved` outcome completed at R7, its record refs at R8); runs started; captured bytes (stored stdout and stderr of every run) | no I/O |
| R4 | choose the **disposition**. In a replay job every class is `keep` (its one iteration is its evidence; no other disposition applies). In a search job: `save` (a failing class while fewer than 20 earlier iterations of the job had a failing class, §7.4: the save attempts are exactly the first 20 failing iterations, the same population as the report's failure refs, whatever their staging outcome); `count` (a failing class otherwise); `keep` (`infrastructure` or `cancelled`: the job stops, and the iteration stays in the job directory as evidence); `discard` (`passed`) | no I/O |
| R5 | `save`, `keep`: enqueue, per run of the iteration, its spec, its record and its first `cleanup.log` (E §6.2, ≤ 24 artefacts, through the queue credits), each with its publication deadline measured from **this** enqueue call (E §6.2), never from its record's finalization; each run's metadata reservation passes to its artefacts. `discard`, `count`: nothing is enqueued and each run's metadata reservation is released unused | E §6.2 credit wait (≤ 1 s per artefact) |
| R6 | wait until every run of the iteration (a transferred one included) has reached **resources released** (E §1 milestone 4; the metadata reservation is settled by R5, §6.2) | the **retirement deadline**, 5 s (*fixed*) after R1; past it §6.4 applies, with the unreleased runs' directories as leftovers |
| R7 | `save`: stage and promote the saved failure (§7.5) | the workspace lock bound (S §4.5, 10 s) plus bounded file work |
| R8 | **settle** the artefacts of R5: wait until each has a result, published or `PersistError` (E §6.2) | the last accepted enqueue call + 5 s |
| R9 | **evict**: drop the iteration's records, encoded specs, summaries, session records and pending persistence results. What remains is R3's aggregates and, for an iteration among the first 20 failing ones, its failure ref with the persisted paths of R8 | no I/O |
| R10 | **remove scratch** with the no-follow walker (S §2): `discard`, `count`: every directory `iterations/<k>.<role>/` of the iteration, completely; `save`: everything in those directories except `spec.json`, `record.json`, `cleanup.log` (the moved files are already in the failure, §7.5); `keep`: nothing | bounded by the file count of one iteration |
| R11 | **release**: once the removal of R10 is confirmed complete, release the retained bytes the job committed for the removed files (E §5.4); the iteration is retired: `iteration_finished` is offered (report contract §7.3) and a progress line (report contract §8) | no I/O |

- **Debug counters** (feature `storage-test-hooks`): live iterations,
  in-memory run records and encoded specs, pending persistence cells,
  queued artefacts, held reservations, and the **persistence tail**
  (below). After R11 of any iteration the first four are 0; queued
  artefacts and held reservations are the job's fixed ones plus the
  persistence tail, which is empty unless a deadline was missed (§6.6).
- **Persistence tail.** When an artefact misses its deadline (R8), E §6.2
  applies unchanged: the thread is marked unavailable, every still-pending
  artefact gets `PersistError{Timeout}` at once, and the write in progress
  together with the reservations of every queued, unwritten artefact stay
  with the thread until that write returns and its owner joins it (late
  completion, settled exactly once; unwritten reservations then released
  uncommitted). The thread is never replaced, so this happens at most once
  per job, and the tail is at most **one retiring iteration's batch**: ≤ 24
  artefacts, their metadata reservations (≤ 8 MiB) and their queue
  charges (≤ Q = 2 MiB, E §6.2). Every later enqueue fails at once with
  `worker_unavailable`, so later iterations, saved ones included, add
  nothing to it: their R5 hand-over is returned at the refusal.
- Because R5–R8 complete before the next admission, at most one
  iteration's artefacts (≤ 24) are ever queued: the stress lifecycle does
  not rely on the persistence queue's headroom (E §6.2 Q). For the credit
  budget B of a stress job, "planned executions" is 6 + 8 × 21 (the
  builds, the ≤ 20 save attempts and the final kept one; R4 bounds the
  save attempts, staging failures included, so no further iteration ever
  persists).
- A persistence deadline missed in R8 marks the thread unavailable for the
  rest of the job (E §6.2): later saved iterations get `PersistError`
  refs, their failures are still staged and promoted, and the late write is
  settled by its joining owner (E §6.2 late completion).

### 6.4 Cleanup failure

If R10 fails to remove an entry (an I/O error, a mount point or a
different device that the walker skips, S §2), or R6 passes its deadline:

1. the job sets the stop reason `cleanup_failed` (§5 row 3), so no further
   iteration is admitted;
2. the bytes of the leftovers stay retained (nothing is released for a
   directory whose removal is not confirmed complete);
3. retirement completes its other steps (artefacts still settle; a saved
   failure still promotes after an R10 failure). After an R6 failure the
   iteration is not staged, since a writer may still change its files: a
   `save` disposition becomes `not_saved{resources_held}`, nothing of the
   iteration is removed, and R10 is skipped;
4. the report lists the leftover paths (job-relative, the first 16 and a
   dropped count, report contract §4.14 `cleanup_failed`);
5. the job verdict is `stress_cleanup_failed` (report contract §2.14),
   exit 10, unless a higher row applies.

Leftovers are pruned with the job directory later (S §6.3).

### 6.5 Cancellation

- **Between iterations**: intent set before the §5 check of iteration k →
  stop reason `cancelled`; no run of k exists.
- **During an iteration**: E §6.4 arbitration applies to every run and
  check of the live iteration: a run whose admission comes after intent is
  `skipped{cancelled}` (never admitted), a running run is stopped (class
  `cancelled`), a finished one keeps its outcome. The iteration's class is
  then `cancelled` (S2, T2) unless S1 / T1 applies; its disposition is
  `keep` and retirement completes.
- **During retirement**: intent ends credit waits (E §6.2: `Cancelled`)
  and changes nothing else; a promotion in progress completes (§7.5 is
  bounded).
- The job verdict follows report contract §2.14: intent that stopped no
  iteration and no run (set after the stop reason was already decided)
  leaves the completed verdict and is recorded (report contract §2.7).

### 6.6 Held-resource bound

At every instant after the builds, the job holds at most:

- one live iteration: ≤ 8 run records and encoded specs (≤ 8 × 320 KiB),
  ≤ 2 executing runs, and ≤ 4 runs holding slots and credits (the
  process-wide bound, E §3.3; a refused admission stops the job by S1/T1);
  none of them survives R6, or the job stops (§6.4);
- reserved bytes: the report reservation 2 × B_S (report contract §3.7)
  plus the live iteration's reservations (streams, metadata, judge
  staging and extraction, transcripts, E §5.4);
- retained bytes in the job directory: the **base** (`job.toml`, lease,
  frozen inputs, captured files, the capture manifest, the builds and their
  persisted metadata) plus, per **save attempt** (≤ 20, whatever its
  staging outcome; R10 keeps the metadata of every `save` disposition,
  `not_saved` ones included, as the evidence its failure ref names), its
  runs' persisted metadata (≤ 8 × 384 KiB, so ≤ 60 MiB in all) and, while
  not promoted, its staged directory (§7.5), plus the one `keep`
  iteration;
- persistence: ≤ 24 queued artefacts of the retiring iteration, or, after
  a missed deadline, the persistence tail instead (§6.3: at most one batch
  per job, ≤ 24 artefacts, ≤ 8 MiB of reservations, ≤ 2 MiB of charges),
  which persists past later retirements until the blocked write returns;
- aggregates of fixed size (report contract §4.14).

None of these grows with the number of passing or counted-only
iterations. §10.1 ST-L06 fixes the ceilings that test this before it runs.

## 7. Saved failures

### 7.1 Id grammar

```
id        = "f-" epoch "-" seq "-" iteration
epoch     = 32 × [0-9a-f]            ; the job's sequence epoch (S §9 encoding)
seq       = "0" | [1-9][0-9]*        ; the job's sequence n, value ≤ u64::MAX
iteration = "0" | [1-9][0-9]*        ; k, value ≤ u64::MAX
```

- At most 76 bytes, ASCII, a portable component (S §2.1) on every OS.
  Splitting at `-` yields exactly four fields, none of which can contain
  `-`, and the epoch has a fixed length, so every id has one parse.
- **Uniqueness**: `(epoch, n)` is allocated once per job by admission
  (S §6.2 step 2a) and `fsck --apply` sequence repair starts a fresh
  random 128-bit epoch at `n = 0` (S §5.6), so ids of jobs before and
  after a repair differ in the epoch; k is unique within a job. Publication
  is no-clobber (§7.5), so an id that exists anyway (a hand-made
  directory) refuses that promotion (`not_promoted{exists}`) and never
  overwrites.
- Readers reject (classification `unexpected`, §7.6): uppercase hex, an
  epoch of another length, a leading zero (`f-…-01-2`), a sign, a value
  above `u64::MAX`, any other character or field count.

### 7.2 Layout

```
problems/<p>/stress/failures/<id>/
    failure.toml                 manifest (§7.3), written last
    input                        the generator's stdout (the iteration's input)
    out/brute                    batch: the brute's stdout, if it started
    out/candidate                batch: the candidate's stdout, if it started
    err/<role>                   stderr capture of each run role that started
    judge/<role>/report          extracted judge report (E §2.1), if one was extracted
    judge/<role>/tout            interactive: an interactor's extracted tout
    transcript/<side>/<name>     interactive relay mode: the session transcript files (<side>: brute, candidate)
    bin/<role>/<path>            preserved program of a role (§7.4), relative paths as captured or built
```

The `<role>` components are the run role tokens of §3.1 (`bin/` uses the
build roles `candidate`, `checker`, `validator`, `interactor`,
`generator`, `brute`). Files absent from an iteration (a run that never
started, a capture without a file) are absent from the directory and from
the manifest. The generator's stdout appears once, as `input`.

### 7.3 `failure.toml`

Tool-written, schema 1, UTF-8 TOML, ≤ 1 MiB (S §9 cap; the writer checks
the rendered size before writing, and a failure whose manifest would
exceed it is `not_saved{manifest_too_large}`). Every `u64` is a decimal
string (S §9 encoding); every native value (argv elements, environment
values, paths outside the failure) is an inline table `{ utf8 = "…" }` or
`{ bytes_b64 = "…" }` with the report contract §3.3 `ns` rule; hashes are
64 lowercase hex digits. Every key below is required unless marked
optional; unknown keys make the manifest `malformed` (it is tool-written).

```toml
schema = 1
id = "f-0123456789abcdef0123456789abcdef-42-17"
problem = "local:a-plus-b"         # problem identity at the job's admission
job = "20261124101500123-0123456789abcdef"
created_at = "2026-11-24T10:15:03.456Z"
cpf_version = "0.3.0"
os = "linux"                       # std::env::consts::OS of the writing cpf
arch = "x86_64"                    # std::env::consts::ARCH
shape = "batch"                    # "batch" | "interactive"
class = "candidate_failure"        # §4 failing class
role = "candidate"                 # deciding role
reason = "wrong_answer"            # §4 reason token
iteration = "17"
start_seed = "25"
check_kind = "compare"             # target.check.kind of the job
source = "…"                       # source fingerprint of the candidate's captured files (S §6.5 encoding)
judge = "…"                        # optional: the judge fingerprint (C §9.1.3), absent for a problem without a judge role
answer = "out/brute"               # optional: present iff the brute answer rule held (§3.5)

[seeds]
generator = "42"
interactor = "1070811988136109722" # optional: only when delivered (C §9.1.6)
solution = "1733034929431036035"   # optional: only when delivered

[[run]]                            # one per admitted run of the iteration, in admission order
role = "generator"                 # run role (§3.1)
stage = "generator"
argv = [{ utf8 = "/…/gen" }, { utf8 = "42" }]   # the resolved program and arguments (E §2)
spec_sha256 = "…"                  # E §2 spec_digest
class = "exited_zero"              # report contract §2.2 class token, or the coverage token for a run without a record class
result = "passed"                  # optional: the judge's own result kind (report contract §4.6 Judge.result), judges only

[[build]]                          # one per role build or build-free / prebuilt role, in §3.1 build order
role = "candidate"
origin = "built"                   # "built" | "build_free" | "prebuilt"
spec_sha256 = "…"                  # optional: the build's spec digest, built roles only
content_sha256 = "…"               # captured files of the role (report contract §4.4 roles[].capture.content_sha256)
program = "bin/candidate"          # optional: the preserved program's directory; absent with `omitted`
omitted = "too_large"              # optional: "too_large" | "job_cap" | "retention_budget" | "io"

[[file]]                           # every file of the directory except failure.toml, in byte order of path
path = "input"
bytes = "123"
sha256 = "…"
```

- `[[file]] path` grammar: `/`-separated components, each a portable
  component (S §2.1); the first component one of `input`, `out`, `err`,
  `judge`, `transcript`, `bin`, with the second component a run role
  (`out`, `err`, `judge`), a side (`transcript`) or a build role (`bin`);
  depth ≤ 16; ≤ 4 096 entries; no two paths with equal collision keys;
  `failure.toml` never listed.
- **Preserved program** of a role (`bin/<role>/…`): a built role's
  validated artifact set (C §6.3, paths relative to the build's output
  root); a build-free role's captured files (paths as captured); a
  prebuilt role's captured program (its captured relative path). Each file
  is listed in `[[file]]`.
- The manifest records what the iteration used; it is not a configuration
  and is never read as one.

### 7.4 Caps

- At most **20** save attempts per job (*default*; R4 `save`): the first 20
  failing iterations, each reported as a failure ref with its outcome
  (`promoted`, `not_promoted`, `not_saved`). Every later failing iteration
  is `count`: counted per class, not persisted, not saved (report contract
  §4.14 `iterations.not_saved`), even when earlier attempts failed to
  stage.
- **Preserved programs**: each role's preserved program is at most
  **64 MiB** (sum of its file lengths), and the job preserves at most
  **256 MiB** of programs in total, over all its saved failures in save
  order and role order. A program over 64 MiB is omitted with `omitted =
  "too_large"`, one beyond the job total with `"job_cap"`, one whose
  reservation is refused with `"retention_budget"`, one whose copy fails
  with `"io"`; the failure is still saved, and the report and `stress ls`
  say that `--replay-binaries` is unavailable for it.
- A reservation refusal or I/O error while staging any file other than a
  program makes the failure `not_saved{retention_budget | io}`: the
  partial staging directory is removed (R10 rules, §6.4 on failure); the
  attempt still counts towards the 20.
- Without `--keep-going` the job stops at its first failing iteration, so
  it saves at most one.

### 7.5 Promotion

The job directory stays leased throughout (the job's own lease, S §6.2),
so pruning cannot remove a staged failure before its promotion or report.

1. **Stage** (lease held, no lock): create `<job>/failures/<id>/`
   (`create_dir`); **move** by rename, within the job directory, the
   iteration's files of §7.2 from `iterations/<k>.<role>/` (stdout and
   stderr captures, `judge/` extracts, transcripts) to their §7.2 names;
   **copy** each preserved program (reflink where supported, else byte
   copy; each file's exact size reserved first, E §5.4), hashing the bytes
   as written; hash each moved file by reading it; write `failure.toml` by
   exclusive creation, last; sync every staged file and then the staged
   directories bottom-up (S §4.3 pre-commit). Crash point
   `stress.failure_staged`.
2. **Lock** the workspace (S §4.5, with leftover recovery; `Busy` →
   `not_promoted{lock_busy}`). Check that `problems/<p>/problem.toml`
   classifies `valid` with the job's problem identity (else
   `not_promoted{problem_changed}`). Create `problems/<p>/stress/` and
   `stress/failures/` with `create_dir` when absent (an existing entry
   that is not a directory, or is a link, → `not_promoted{path_blocked}`).
   Crash point `stress.failure_parents`.
3. **Publish** with `publish_dir_noclobber` (S §4.3) from
   `<job>/failures/<id>/` to `problems/<p>/stress/failures/<id>/`; `Exists`
   → `not_promoted{exists}`, `CrossFilesystem` →
   `not_promoted{cross_filesystem}`, `Unsupported` →
   `not_promoted{unsupported}`, another error → `not_promoted{io}`. The
   rename is the **commit point** (crash points `dir_noclobber.renamed`,
   `stress.failure_published`); post-commit `Durability::Uncertain` or
   `Cleanup::Pending` are `durability` warnings. Unlock.
4. On commit, release the retained bytes of the moved and copied files:
   they are user files now, outside every retention budget (S §3, §6.4).
   On any `not_promoted` outcome the staged directory stays in the job
   directory, its bytes stay retained, and the report names its
   job-relative path; it is pruned with the job (S §6.3).

One attempt per failure; no retry. A crash before the commit leaves the
staged directory in the (now inactive) job directory, pruned with it, and
nothing under `problems/`; after the commit the published directory is
complete (S §10 rows). Once published, cpf never
modifies, renames or deletes a saved failure; the user may.

### 7.6 Readers (untrusted input)

A saved failure is user-owned and may be edited, replaced or crafted, so
every reader treats it as untrusted:

- **Entry classification** (by `stress ls`, `--replay`,
  `--replay-binaries`, `test add --from-stress`, `fsck`): an entry of
  `stress/failures/` whose name is not an id (§7.1), or that is not a
  directory (checked without following links), is `unexpected`.
  Otherwise its `failure.toml` is classified by the S §9 tool-only record
  classification (`missing`, `unreadable`, `malformed`, `too_new`,
  `valid`), opened without following links, bounded at 1 MiB + 1 byte;
  `valid` additionally requires every §7.3 rule, `id` equal to the entry
  name, and `problem` equal to the selected problem's identity (else
  `malformed` with the reason).
- **At use** (replay, from-stress): every file the operation needs is
  opened without following links, its type checked on the opened handle
  (a regular file), and copied into the job (or the private capture of
  `test add`) while its SHA-256 and length are computed; a mismatch with
  `[[file]]`, a missing file, a link or a non-regular file refuses the
  operation (class `workspace`, exit 8) before any run, with the path and
  reason. Files not listed in `[[file]]` are ignored, never read.
- **Nothing in a failure directory is ever executed.** `--replay-binaries`
  executes only the verified copies inside the replay job (§8), on
  explicit request; no other reader executes anything.

## 8. Replay

A replay job has kind `stress` and mode `replay` or `replay_binaries`. It
reruns one saved failure: exactly one admission (a = 0 → 1), whose
iteration number is the saved k (any `u64`, `u64::MAX` included: it names
the run directories `iterations/<k>.<role>/` and the events), with the
saved seeds. The search budgets, the seed-range check of §2.1 and §5 rows
4–6 do not apply; its stop reason is `replay` (§5 row 7) unless row 1–3
applies; the report's `budget` is `null` and `seeds.start` is the saved
generator seed. It never saves or promotes failures.

- **Selection and validation**: after the platform boundary (§2.5), the
  failure must classify `valid` (§7.6; else exit 8) and its `shape` must
  equal the current effective kind's (`batch` / `interactive`; else exit 7,
  "the failure was saved for an <shape> problem").
- **Input**: for a saved `generator_failure` the generator runs with the
  saved generator seed; for every other class the generator is not run, and
  the saved `input` is frozen into the job as `inputs/replay/input`
  (verified copy, §7.6, reserved as a frozen input, S §6.4) and used as
  the iteration's input. `from_input` in the report says which.
- **Roles**: `--replay` builds and runs the **current** sources and
  configuration of every role (§3.1), exactly as a search job would, with
  the saved interactor and solution seeds delivered as the current
  declaration allows (C §9.1.6).
- **`--replay-binaries`**: no build runs. Refused (exit 7, before
  admission, naming the reason) when the manifest's `os` or `arch` differ
  from the running cpf's, or when a role the current plan needs has no
  preserved program (`omitted`, or absent). Each needed preserved program
  is verified and copied (§7.6) into `inputs/bin/<role>/` (each file
  reserved, exclusive creation; Unix mode `0700` for the files the run
  stage executes: a built role's `file` artifact, a prebuilt program), and
  the current run stages are composed with `{artifact}` /
  `{artifact_dir}` (built), `{sources}` / `{source}` (build-free) or the
  program (prebuilt) naming those copies. The run argv therefore comes
  from the current configuration, not from the manifest; the report says
  `binaries: true`, `executions[]` has no build, `roles[].build` is
  `null`.
- **Reproduced** iff the replay iteration's class equals the saved
  `class` (report contract §4.14 `replay.reproduced`). A replay that
  passes reports "not reproduced"; one that fails with another class
  reports both classes. Neither is a determinism, acceptance or timing
  claim (checkers design §7).
- The status follows the replay iteration's class (report contract
  §2.14): a reproduced candidate failure exits 1, a passing replay 0.

## 9. Report, events and human output

The stress job report is `job_report` with `command: "stress"` and the
member `stress` (report contract §4.14); its verdict and exit status are
report contract §2.14 and §5; its size bound is the fixed **B_S = 1 MiB**
(report contract §3.7), reserved twice before the first run (2 MiB).
Builds appear in `executions[]` and `roles[]`; `tests[]` and
`aggregates[]` are empty; `fact` is `null` and no hook is planned (phases
P, V, W, E, F, S, D; report contract §2.9).

- **Diagnostics** (R3; the first 16 per class, each ≤ 1 KiB of text):
  the deciding run's evidence text: a judge's message (report contract
  §4.6), else the deciding run's stderr head, else the comparator's `Diag`
  rendered as in human output; `source` names which. `passed` has none.
- **Events** (report contract §7.3): `iteration_started`,
  `iteration_run_started`, `iteration_run_finished`, `iteration_finished`
  carry `u64` iterations and `<k>.<role>` ids and no `r` id; lossy like
  every non-terminal event (report contract §7.4).
- **Human output** (report contract §8): iteration updates are progress
  lines (lossy; at most one per 250 ms, plus one per failing iteration);
  the final aggregates are result lines, at most 128, inside the 128 KiB
  final allowance.

## 10. Tests

Every row is one named test. **Injected** rows run the job engine with an
injected executor (each run's outcome scripted: class, exit, captured
bytes written through the engine's own file sinks, durations from a
controlled clock), on Linux, macOS and Windows, so files, bytes and
reservations are real while no process starts. **Real** rows run real
fixture programs on Linux and macOS; on Windows the same command lines
assert the refusal (status 10, no job directory). Each row asserts exact
counts (iterations admitted and retired, runs started, saved and counted
failures, files), the content read back, and the status. Injection
points: `stress.iteration_admit` (before the §5 check), `stress.run_admit`
(before each iteration run's admission), `stress.retire` (before each R
step, with its name), `stress.remove` (scripted removal failure of a
chosen entry), `stress.promote` (before step 2 of §7.5), `stress.clock`
(the controlled monotonic clock of the time budget), `stress.counters`
(initial cumulative counter values, debug builds only), plus those of
report contract §9.

### 10.1 Lifecycle and plateau (injected, all OSes)

| Id | Fixture | Expected |
|---|---|---|
| ST-L01 | batch, 5 runs per iteration, 100 passing iterations; interactive, 8 runs, 100 passing iterations; then one batch iteration with an explicit hold schedule: the injected executor finalizes the records of `generator`, `validator` and `brute` at controlled-clock times 0, 1, 2 s while each run's resources (stderr sink and slot) stay held behind one shared barrier H; H stays closed until the candidate's admission has acquired its slot (the fourth); the harness then samples the occupancy and only after that releases H; the candidate's record is finalized at 3 s | admitted 100, retired 100, runs started 500 / 800; at every `stress.run_admit` barrier the live-iteration counter is 1 and no run of the next iteration exists; never more than 2 iteration runs executing at once; in the scheduled iteration the occupancy sampled after the candidate's slot acquisition and before H opens is exactly 4 (3 holders and the candidate), all four release within R6's 5 s after R1, the iteration retires normally, and the next iteration is admitted only after the occupancy is 0 |
| ST-L02 | intent at `stress.iteration_admit` of k = 7 | stop `cancelled`, retired 7 (0…6), no directory `iterations/7.*`, verdict `cancelled`, cancellation status |
| ST-L03 | intent at `stress.run_admit` of `7.candidate` (batch) and of the candidate pair (interactive) | the candidate `skipped{cancelled}`; class `cancelled` (S2/T2); disposition `keep`: `iterations/7.*` kept with persisted spec, record and `cleanup.log` of each admitted run; retired 8; no further admission |
| ST-L04 | intent while `7.candidate` runs (scripted long run) | the run stopped (class `cancelled`), iteration `cancelled`, as ST-L03 |
| ST-L05 | intent at `stress.retire` R7 of a saved failure | the promotion completes (`promoted`), settlement completes, no further iteration |
| ST-L06 | **plateau**: batch, every iteration passes, each run writing 4 KiB stdout and 1 KiB stderr, 10 000 iterations; samples at quiescence after R11 of k = 999, 1 999, …, 9 999 (10 samples) | retired exactly 10 000, runs started exactly 50 000; at all 10 samples: the file count and the logical bytes under the job directory equal the first sample's exactly; retained bytes equal the first sample's and reserved bytes equal 2 × B_S exactly; in-memory records, encoded specs, pending persistence cells and queued artefacts 0; cleanup slots held 0, thread credits held equal to the job's own 2 (E §3.3), cleanup-service occupancy 0; thread count (Linux `/proc/self/task`, macOS `task_threads`, Windows Toolhelp thread snapshot) and open descriptors (Linux `/proc/self/fd`, macOS `/dev/fd`, Windows `GetProcessHandleCount`) equal the first sample's; resident memory (Linux `VmRSS`, macOS `task_info` resident size after the E §10.2 validation, Windows working set) ≤ first sample + 16 MiB; the report has `iterations.retired = "10000"`, one class `passed` with count `"10000"`, no failure refs, size ≤ B_S |
| ST-L07 | plateau, interactive shape (8 runs, transcripts reserved per session), 10 000 iterations | as ST-L06 with 80 000 runs |
| ST-L08 | plateau with failures: `--keep-going`, every iteration `candidate_failure`, 10 000 iterations | saved exactly 20 (k = 0…19, all `promoted`), `not_saved` 9 980; from the first sample on, files and bytes in the job directory equal (the base plus 20 × the persisted metadata of 5 runs); failure refs 20, `failures_dropped` 9 980; exactly 20 directories under `stress/failures/` |
| ST-L09 | removal failure injected at R10 of k = 5 (one entry of `iterations/5.candidate/cwd/`) | no iteration 6 admitted (admitted 6, retired 6), stop `cleanup_failed`, verdict `stress_cleanup_failed`, status 10, `cleanup_failed.paths` names exactly that entry, its bytes still retained; a later `cpf clean --apply` prunes the job |
| ST-L10 | R6 deadline: `5.candidate`'s stderr writer stalled (controlled sink, released after 8 s) | the record is finalized with `write_stalled` on stderr (class unchanged), retirement waits 5 s [+ 100 ms] after R1, stop `cleanup_failed`, the run's directory listed; the job still reaches `JobFinished` |
| ST-L11 | engine debug counters after every R11 in ST-L01 | live iterations, records, specs, pending cells, queued artefacts and the persistence tail 0; persistence never holds an artefact of an earlier iteration |
| ST-L23 | persistence tail: `--keep-going`, every iteration failing; the controlled writer blocks the **spec** write of `2.generator` (the first artefact of iteration 2) until released, so the other 14 artefacts of iteration 2 are queued behind it; 10 more iterations run, then the writer is released | iteration 2's artefacts all `timeout` at the missed deadline; iterations 3–12 continue (saved, refs `worker_unavailable`, failures still staged and promoted); after each later R11 the tail counters are exactly iteration 2's batch (15 artefacts held, their 5 MiB of metadata reservations and their queue charges) and nothing else is queued or reserved beyond the fixed part; on release the late write is settled once (committed by its joining owner, never twice), the 14 unwritten reservations are released uncommitted, and reserved bytes return to the fixed baseline |
| ST-L12 | slow controlled writer (each artefact 200 ms), `--keep-going`, 25 failing iterations | every artefact of the 20 saved iterations published (no `queue_full`, `timeout`, `cancelled`), max queued artefacts ≤ 24 and of one iteration only; the 5 counted iterations enqueue nothing |
| ST-L13 | counters: `stress.counters` sets runs-started to `u64::MAX − 3` before a batch iteration | stop `counter_limit` before that iteration, no run admitted, nothing wraps |
| ST-L14 | time budget with `stress.clock`: T = 10 s, each iteration 3 s | admitted exactly 4 (checks at 0, 3, 6, 9 s), the fourth completes past T, stop `time_budget` |
| ST-L15 | iteration budget N = 3 and N = 1 | retired exactly 3 / 1, stop `iterations` |
| ST-L16 | event identities beyond 10 000: 10 005 passing iterations with an events consumer | `iteration_run_started.run` of k = 10 004 is `"10004.candidate"`; `run_started` only for builds (`r00000`…); `seq` strictly increasing; `executions[]` holds only builds; report ≤ B_S |
| ST-L17 | retention refusal: job budget sized so that `3.brute` is refused | `not_started{retention_budget}`, class `infrastructure`, stop `admission_closed`, iteration kept |
| ST-L18 | delayed persistence: a saved iteration whose generator record is finalized at t = 0 (controlled clock) and whose candidate runs 6 s | every artefact of the iteration published, none `timeout`; each deadline is its R5 enqueue + 5 s; the persistence thread stays available for the next saved iteration |
| ST-L19 | transferred creation: injected blocked creation of the candidate pair (interactive) with intent set, and of a follow-up checker | R1 accepts the cancellation snapshot at once (no reduction, no wait for creation); class `cancelled`; R6 waits for the transferred runs' resources up to 5 s, then the job stops; verdict `cancelled` (precedes `stress_cleanup_failed`) |
| ST-L20 | metadata reservations: 10 passing iterations, then one saved | after each passing R5 the job budget's reserved bytes drop by exactly 5 × 1 MiB; for the saved one they drop as each artefact settles; never more than 8 MiB of metadata reserved |
| ST-L21 | cleanup `unverified` (injected) for every candidate run, 100 iterations | classes unchanged (`passed`), note `cleanup_unverified`, retirement proceeds, retired 100 |
| ST-L22 | lightweight long plateau: batch, zero-byte captures, 1 000 000 iterations, samples every 100 000 | the ST-L06 equalities at all 10 samples; `iterations.retired = "1000000"`; report ≤ B_S |

### 10.2 Seeds, roles, classes, budgets (real, Linux and macOS)

| Id | Fixture | Expected |
|---|---|---|
| ST-R01 | generator printing its argv; `--seed 5 --iterations 4` | generator argv ends with `5`, `6`, `7`, `8` exactly, in order; report `seeds.start` `"5"` |
| ST-R02 | `--seed 18446744073709551614 --iterations 2`; `--iterations 3` | the first admitted (seeds …614, …615); the second exit 7 before admission, no job directory |
| ST-R03 | no `--seed` | a start seed in [0, 2⁶³ − 1] reported and printed; rerun with `--seed` of that value gives the same generator argv sequence |
| ST-R04 | every role built from C++ sources | builds in §3.1 order, each exactly once per job (6 `executions[]`), none per iteration |
| ST-R05 | a generator syntax error | `build_failed`, status 3, no iteration admitted, stop `build_failed` |
| ST-R06 | generator exits 1; dies by a signal; exceeds its stdout cap; times out | `generator_failure` with reason `nonzero_exit`, `signal`, `output_budget`, `wall_deadline`; the brute and candidate never started |
| ST-R07 | `simple` validator exiting 1; `testlib` validator exiting 3 | `validator_failure`, `invalid_input` / `validation_failed`; status 5 |
| ST-R08 | brute exiting 1; brute printing a non-number under the `float` comparator | `oracle_failure`, `nonzero_exit` / `invalid_answer`; the candidate never started; status 5 |
| ST-R09 | `testlib` checker exiting 3 (`fail`); checker by a signal | `judge_failure`, `declared_fail` / `signal`; status 5 |
| ST-R10 | candidate wrong on seed 3 only, kind `compare`; candidate TLE; candidate WA by a `testlib` checker; PE by the checker; scored 50 with `min_score = "60"` | `candidate_failure` at k = 3 with the `Diag` reason; `wall_deadline`; `wrong_answer`; `presentation_error`; `score_below_threshold`; status 1 each |
| ST-R11 | constructive problem: no brute, kind `checker`, `answer = "none"` | the checker receives an empty `answer`; passing and failing candidates classified S8 / S7 |
| ST-R12 | the brute answer rule: brute with a complete output and a validator rejecting the input | the brute never started (S4 first); with a valid input, `answer` staged equals `out/brute` byte for byte |
| ST-R13 | 30 iterations, candidate failing at k = 4 and 9, without and with `--keep-going` | without: stop `failure` after k = 4, retired 5, one saved; with: retired 30, saved 2, class counts exact |
| ST-R14 | a passing candidate, 200 iterations | status 0, verdict `passed`, nothing under `stress/failures/`, no `iterations/` entry left in the job directory |
| ST-R15 | time budget 1 s with a 300 ms brute | stop `time_budget`; retired ≥ 1; the job reaches V within 1 s + one iteration + 5 s [+ 500 ms] |
| ST-R16 | real plateau: trivial generator, brute and candidate, 2 000 iterations, samples every 500 | as ST-L06 (files, bytes, retained, reserved, descriptors exact), resident memory ≤ first sample + 32 MiB |
| ST-R17 | human output with `stderr.write` blocked from the first iteration, released 1 s after t_q, 2 000 iterations, `--keep-going`, 3 failures | stderr ends with the stress result lines in order (stop line, class lines, 3 failure lines, timing lines, the summary last); `progress_dropped.lines` > 0; no result line missing (report contract §9.3 stress case) |
| ST-R18 | stress on a problem with `[config] io` | exit 7 before admission ("unsupported in this version") |
| ST-R19 | `[stress]` without `generator`; a `profile` or `seed` in a stress role spec; `iterations = 0`; `time_s = 0`; an unknown key; no brute on a batch `compare` problem; `min_score` with `pass_fail`; `min_score` above the scale | `cpf stress` and `cpf config check` exit 7 with a diagnostic naming `stress.<key>` (phase 1 or 2 as C §9.2); the same problem's `cpf test` runs normally (an invalid `[stress]` is not read by it) |
| ST-R20 | candidate stdout 64 MiB + 1 byte under the default cap; generator writing exactly its cap | `candidate_failure{output_budget}`; the generator's input of exactly 64 MiB is used (complete capture) |

### 10.3 Saved failures and promotion

| Id | Fixture | Expected |
|---|---|---|
| ST-F01 | one candidate failure (batch, checker) | `problems/<p>/stress/failures/<id>/` with `failure.toml`, `input`, `out/brute`, `out/candidate`, `err/<role>` for the 5 runs, `judge/checker/report`, `bin/<role>/…` for 5 roles; every `[[file]]` hash and length equals the file; `seeds.generator` = g_k; `answer = "out/brute"`; the job directory keeps only spec, record and `cleanup.log` of `iterations/<k>.*` |
| ST-F02 | id grammar vectors | accepted: `f-` + 32 hex + `-0-0`, `-18446744073709551615-18446744073709551615`; rejected: uppercase hex, 31 or 33 hex digits, `-01-`, `-+1-`, `-18446744073709551616-`, five fields, a trailing `-` |
| ST-F03 | two concurrent stress jobs on one problem, each saving 2 failures | 4 distinct ids, distinct `n`, all `promoted` |
| ST-F04 | `fsck --apply` sequence repair between jobs: jobs n = 3, 4 of epoch E₁ save failures; `job-seq` removed; `cpf fsck --apply` (new epoch E₂, `next = 0`); a job saves at k = 0 | ids `f-E₁-3-…`, `f-E₁-4-…`, `f-E₂-0-0` all distinct; the old directories byte-unchanged; `stress ls` lists 3 `valid` entries |
| ST-F05 | a hand-made directory with the id the next failure will get | `not_promoted{exists}`, the existing directory byte-unchanged, the staged directory named (job-relative) in the report |
| ST-F06 | `--keep-going`, 25 candidate failures | saved exactly 20 (k = first 20 failing), the 21st and later counted (`not_saved` 5), no staging left for them |
| ST-F16 | failed save attempts: `--keep-going`, 25 failing iterations, staging of the first 20 failing with injected `EIO` | 20 failure refs, each `not_saved{io}` with record refs naming kept `iterations/<k>.<role>/record.json`; the job directory keeps exactly the persisted metadata of those 20 iterations (5 runs × 3 files each) and nothing staged; iterations 21–25 `count` (no staging attempt, nothing enqueued, directories removed); `not_saved` 25; files and bytes in the job directory equal after iteration 20 and after 25; queued artefacts never exceed one iteration's |
| ST-F07 | a candidate program of 64 MiB + 1 byte (padded binary) | `omitted = "too_large"` for the candidate, the failure saved; `stress ls` and the report say binaries are unavailable; `--replay-binaries` refused, exit 7 |
| ST-F08 | 5 failures with 60 MiB of programs each | programs preserved for the first 4 failures (240 MiB), the fifth `omitted = "job_cap"` (256 MiB cap); all 5 saved |
| ST-F09 | `ENOSPC` injected while staging `out/candidate` (disk full during promotion); and at the publish rename | the first `not_saved{io}`, partial staging removed; the second `not_promoted{io}`, staged directory kept in the job directory; both reported; the job lease is held until `report.json` is written (a concurrent `cpf clean --apply` at `persist.report` skips the job) |
| ST-F10 | workspace lock held by another process for 11 s at `stress.promote` | `not_promoted{lock_busy}`, staged directory kept and named |
| ST-F11 | the problem directory removed at `stress.promote` | `not_promoted{problem_changed}`; nothing created under `problems/` |
| ST-F12 | `problems/<p>/stress` is a regular file; a symlink | `not_promoted{path_blocked}`; the file or link unchanged |
| ST-F13 | `stress ls` on Windows over a workspace with saved failures (created on Linux, copied) | lists each entry `valid`, status 0 |
| ST-F14 | `stress ls` with entries: a valid failure, `notes.txt`, an id-named file, a directory with an uppercase id, a manifest with `schema = 2`, one of 1 MiB + 1 byte, one whose `id` differs from its name, one with an unknown key, one with `[[file]] path = "../x"`, a symlinked `failure.toml` | `valid`, `unexpected` × 3, `too_new`, `malformed` × 4 (size, id, key, path), `unreadable` (link) ; exit 0 |
| ST-F15 | manifest size: a failure whose manifest would exceed 1 MiB (injected huge argv) | `not_saved{manifest_too_large}` |

### 10.4 Replay and `--from-stress`

| Id | Fixture | Expected |
|---|---|---|
| ST-P01 | saved deterministic candidate failure, `--replay` | one iteration, k as saved, `from_input: true`, the generator not run, `reproduced: true`, status 1 |
| ST-P02 | the candidate fixed after saving, `--replay` | class `passed`, `reproduced: false`, status 0, human "not reproduced" |
| ST-P03 | nondeterminism, scripted: a candidate that fails iff a control file the harness writes says `fail`; saved with `fail`, replayed with `fail`, then with `pass` | `reproduced: true`, then `reproduced: false` (class `passed`, status 0); no output line claims determinism, a fix or timing equivalence |
| ST-P13 | replay of saved failures with k = 9 999, 10 000 and `u64::MAX` (default `iterations` 10 000 in `[stress]`), each (a) reproduced (candidate still failing), (b) passing, (c) with the candidate now failing to build | (a), (b): exactly one iteration admitted with that k (`iterations/<k>.candidate/`), disposition `keep`, stop `replay`, `budget: null`, `reproduced` true / false, status 1 / 0; (c): no iteration admitted, stop `build_failed`, status 3 |
| ST-P04 | saved `generator_failure`, `--replay` | the generator runs with the saved seed (`from_input: false`) |
| ST-P05 | `--replay-binaries` of a candidate failure, sources then edited to fix it | no build in `executions[]`; the preserved candidate reproduces (`reproduced: true`); the copies in `inputs/bin/` are byte-equal to the manifest |
| ST-P06 | `--replay-binaries` with the manifest's `os` or `arch` edited | exit 7 before admission, naming both values |
| ST-P07 | hand edits: `[[file]] path = "../../../tests/x"`; `input` modified (hash mismatch); `input` replaced by a symlink; `bin/candidate/sol` replaced by another binary | path: `malformed`, exit 8; hash: exit 8 before any run, nothing executed (the replacement binary never runs: a sentinel file it would create stays absent); link: exit 8 |
| ST-P08 | replay of an interactive failure into a problem now batch | exit 7 |
| ST-P09 | `cpf test add --from-stress <id>` of a batch failure with `answer` | `tests/stress-1.in`, `.ans` byte-equal to `input`, `out/brute`; entry `origin = "stress"`; status 0; on Windows too |
| ST-P10 | `--from-stress` of an `oracle_failure` (no `answer` key) and of an interactive failure | input registered without answer |
| ST-P11 | `--from-stress` with a modified `input` | exit 8, nothing published under `tests/` |
| ST-P12 | Windows: `cpf stress --replay <id>` and `--replay-binaries <id>` with a valid configuration; with a `[stress]` phase-1 error | status 10; status 7 |

### 10.5 Crash states (storage crash harness, real `cpf`, Linux and macOS)

Each kill asserts the recovered content, not only the status.

| Id | Crash point | Expected after the kill |
|---|---|---|
| ST-C01 | during an iteration (scripted long candidate run) | nothing under `problems/<p>/stress/`; the job directory inactive, pruned by the next admission or `clean --apply` |
| ST-C02 | `stress.failure_staged` | as ST-C01; the staged `<job>/failures/<id>/` complete, removed with the job |
| ST-C03 | `stress.failure_parents` | `stress/failures/` exists and is empty; a later job promotes into it |
| ST-C04 | `dir_noclobber.renamed`, `stress.failure_published` | the failure directory complete under `problems/`: `failure.toml` valid, every `[[file]]` hash matches, `stress ls` `valid`; nothing of it left in the job directory |
| ST-C05 | `stress.iteration_persisted#k` (after R8 of a `keep` iteration) | the job directory holds the iteration's persisted records; `fsck` reports nothing about `stress/` |

### 10.6 Interactive stress (real, Linux and macOS)

| Id | Fixture | Expected |
|---|---|---|
| ST-I01 | pure: every session verdict of report contract §2.12 for the brute session and for the candidate session, with and without a follow-up checker | exactly one class per combination by T1–T7 (table-driven, all OSes) |
| ST-I02 | adaptive guess-the-number interactor with `seed = "env"`, a correct brute, a candidate wrong for some seeds | `candidate_failure` saved with `seeds.interactor` = `seed(interactor, g_k)`; both sessions received the same `CPF_SEED_INTERACTOR` |
| ST-I03 | a brute that fails the predicate (WA) | `oracle_failure`, never `candidate_failure`; the candidate session never started |
| ST-I04 | interactor `fail` in the brute session | `judge_failure`, role `brute_interactor` |
| ST-I05 | a candidate failure saved and replayed through §8 | `reproduced: true`; `judge/candidate_interactor/tout`, `transcript/candidate/…` saved and listed |
| ST-I06 | `[check] answer = "file"` on an interactive problem | `cpf stress` exit 7 before admission |
| ST-I07 | each of the classes generator, validator, judge (checker side), oracle, candidate, pass in one job with `--keep-going` | class counts exact; runs per iteration ≤ 8 |

## 11. Amendments to other documents

Applied with this contract, in place:

- **E** §1 (iteration, iteration run), §2 (stages `Generator`, `Brute`),
  §3.3 (one live iteration), §5.4 (release after confirmed removal), §5.5
  (retirement deadline), §6.2 (no spec at admission for iteration runs,
  per-iteration settlement, credit budget of a stress job), §9 (iteration
  events).
- **C** §1.1 (`[stress]` beside `[check]`), §8.2 (stress plans), §9 (the
  `cpf stress` row), §9.2 (`[stress]`).
- **Report** intro and §1 (job kind `stress`), §2.14 (stress verdict),
  §3.2 (types `irid`, `fid`), §3.4 (run ids), §3.7 (B_S), §4.1, §4.4
  (roles `generator`, `brute`), §4.14 (`stress` member), §4.15
  (`stress_failures` document), §5.1–5.5 (exit rows), §6.1, §7.3
  (iteration events), §8 (stress lines), §9.7 (fixtures).
- **S** §3 (ownership of `problems/<p>/stress/failures/`), §5.3
  (`--from-stress`), §5.5 (gitignore comment), §5.6 (fsck), §6.1 (job
  layout), §6.4 (release after removal, promotion), §9 (`failure.toml`),
  §10 (crash rows).
- **Design §6.5**, ratified in the design: "Generator, oracle and checker
  failures are saved too" is narrowed to the caps of §7.4 under
  `--keep-going`; infrastructure and cancelled iterations are never saved
  (§6.3 R4) and stop the job.

## 12. Caveats and rationale

- **Why one live iteration.** Overlapping iterations would need a second
  set of slots, reservations and persistence work and would make the
  resource bound depend on scheduling; the CLI foundation runs tests
  sequentially too (E §3.3).
- **Why retirement waits for resources released.** Removing a directory
  whose writer is still running would release bytes that the writer then
  commits again (E §5.4); a stalled writer therefore stops the job
  (§6.4) rather than letting accounting drift.
- **Why derived interactor and solution seeds.** Drawing them from the OS
  would make a search run unrepeatable from its start seed and would need
  a separate draw shared by both sessions; derivation from `g_k` gives both
  properties and is recorded anyway.
- **Why infrastructure stops `--keep-going`.** An observation cpf could not
  make reliably once (a cleanup slot leak, a disk error) is likely to
  repeat; continuing would bury the first evidence under thousands of
  copies.
- **Why staging inside the job directory.** `.cpforge/staging/` is emptied
  by every locked operation's recovery (S §4.5), so a failure cannot be
  built there outside the lock; the job's own leased directory survives
  until the job ends and is pruned with it after a crash.
- **Saved failures can be large** (inputs and outputs up to their caps,
  programs up to 256 MiB per job). They are user work and never pruned;
  `cpf gitignore` offers them only as a comment (S §5.5).
