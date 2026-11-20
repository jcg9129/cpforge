# Checkers and interactors (design)

Status: **v7.1, frozen** (dialogue-cap amendment §5.5). Covers custom
checkers per problem, adaptive/adversarial interactors and two programs
communicating. Supersedes design §6.4.

Feature scope: checkers and interactive judging follow the CLI foundation;
the connected-run mechanics (§5) belong to the supervisor and record events
and provenance but derive no verdicts (§11). Supervisor mechanics
(ownership, deadlines, capture, cleanup, persistence, cancellation arbitration)
are `contracts/execution.md` (cited *E §n*) and are not restated; comparators
are `compare-spec.md`; CLI exit codes belong to the report transport.

This is cpforge's **local policy**, not a claim about how any judge decides.

## 1. Roles and result model

| Role | Runs | Decides |
|---|---|---|
| Comparator | built in | output vs answer |
| Checker | user program, after S finished | input, output, answer |
| Interactor (I) | user program, concurrently with the solution (S), connected to it | the dialogue; may be adaptive |
| Validator | user program, on test input | input well-formedness |

Every role is an ordinary execution spec (configuration contract): built from source
(`checker = { sources = ["checker.cpp"], language = "cpp", profile = "testlib" }`)
or a prebuilt path; never from scraped data (design §7).

Every checker-like result is normalised into one model:

```rust
pub enum CheckResult {
    Passed,
    Failed { kind: FailKind },               // WrongAnswer | PresentationError
    Scored { raw: Decimal, scale: Decimal },  // §4
    Unchecked { reason: UncheckedReason },   // NoAnswer, CheckingUnverified, CheckingInterrupted
    CheckerFailed { class: CheckerFailClass },
}
pub enum CheckerFailClass {
    DeclaredFail,         // testlib FAIL / exit 3
    ProtocolUnsupported,  // outcome, exit code or attribute outside the profile
    ReportInvalid,        // missing, malformed, oversized, undecodable, disagreeing report
    ArtefactInvalid,      // tout missing or oversized (§6.4)
    Crashed,              // signal, own limit, peer-allowance expiry
    TestDataError,        // answer/validator problems detected by cpf
}
```

`CheckerFailed` is never WA and never a zero score; execution evidence of
every process is kept beside the result (E §6).

## 2. Invocation profiles

### 2.1 `testlib` checkers and interactors

```
checker    <input> <output> <answer> <report> -appes
interactor <input> <tout>   <answer> <report> -appes
```

- The report arguments are **always** passed; an authoritative testlib
  interactor without them does not exist in cpf. `<report>` is a
  fresh path in the run's private directory (storage contract).
- **Tested header:** the profile is pinned to one `testlib.h` version,
  recorded in `docs/testlib-profile.md` by the initial feature release task and exercised in
  CI. A checker cpf builds records the hashes of its source and of the header
  it compiled against, the compiler and flags; a prebuilt checker is
  `provenance = "declared"`.
- **Observable outcome matrix**. cpf sees only the exit code and the
  report; it never infers which source call produced them. Rows are
  observable combinations for the tested header:

  | Exit | APPES `outcome` / attributes | cpf |
  |---|---|---|
  | 0 | `accepted` | `Passed` |
  | 1 | `wrong-answer` | `Failed{WrongAnswer}` |
  | 2 | `presentation-error` (also the default header's unexpected EOF) | `Failed{PresentationError}` |
  | 3 | `fail` (also what `quitpi()` produces: the header's unset-points failure) | `CheckerFailed{DeclaredFail}` |
  | 7 | `points` with a `points` attribute | `Scored` (§4), only if scoring is enabled |
  | `PC_BASE_EXIT_CODE + n` | `partially-correct` with `pctype` | `CheckerFailed{ProtocolUnsupported}` |
  | any other code | any | `CheckerFailed{ProtocolUnsupported}` |

  `points` is the attribute value **as received** (the header rounds when
  emitting). A prebuilt binary built with custom exit remaps or the
  unexpected-EOF option cannot be detected from its output: the profile is a
  **declared default profile**, and such builds are a provenance restriction
  stated in the report (`provenance = "declared"`), not a detected error.
- **Report:** read with a bounded read (at most 64 KiB + 1 byte, §5.5).
  Accepted encodings are the declared `windows-1251` (testlib's default) and
  `UTF-8`. The message (4 KiB kept) comes from the report; stderr is
  diagnostics only. **No fallback to stderr.**
- **Staged validation**; the first failing stage decides, and nothing
  is inferred from an invalid or partial document:
  1. execution facts of the judge process (own limit, signal,
     peer-allowance expiry → `Crashed`; a judge that cannot start is
     infrastructure, E §6.4), decided before reading anything (§6.1 rule 5);
  2. report present, size ≤ 64 KiB, declared encoding decodable →
     else `ReportInvalid`;
  3. XML well-formed → else `ReportInvalid`;
  4. schema: known element, known `outcome` value, attributes known for it,
     `points` matching §4's grammar → else `ReportInvalid`;
  5. support, checked **separately**: the exit code is one the profile
     supports (0, 1, 2, 3, 7), and the outcome is one it supports and the
     mode allows (§6.3); either unsupported → `ProtocolUnsupported`;
  6. agreement: the supported exit code and the supported outcome form a row
     of the matrix (e.g. exit 0 + `wrong-answer` → disagreement) → else
     `ReportInvalid{Disagreement}`.

### 2.2 `simple` checkers

`checker <input> <output> <answer>`. Exit 0 = OK, 1 = WA; other codes,
signals, own limits → `Crashed`. Message: stderr (1 KiB kept). Exit status
cannot reveal intent (an unhandled exception with exit 1 looks like WA); the
profile description says so, and authors who need the distinction use
`testlib` or a wrapper. Not available for interactors.

### 2.3 Validators

Own profiles; invalid input is `TestDataError` for the test, never a solution
verdict:

- `testlib-validator`: input on stdin, exit 0 valid; anything else is
  "validation failed (invalid input and validator error are
  indistinguishable)", with stderr kept.
- `simple-validator`: `validator <input>` and stdin; exit 0 valid, 1 invalid,
  anything else validator error.

### 2.4 Other conventions and paths

Kattis output validators and DMOJ checkers are later adapters onto
`CheckResult`; until then a wrapper run as a custom command translates to a
supported profile. Library Checker uses testlib. All paths are
absolute inside the run's private directory, which is also the `cwd`;
declared assets are copied in. This is wiring, not isolation.

## 3. Answers

- `answer = "file"` (default): a missing `.ans` for an enabled test is a
  configuration error before anything runs.
- `answer = "none"`: an explicit author assertion that the selected checker
  accepts an empty answer; cpf passes an empty file. Rejected for the
  built-in comparators.
- Absent, deliberately none and legitimately empty are three states in
  `tests.toml`.

## 4. Scores

- `scoring = "pass_fail"` (default) or `{ points = true, scale = S,
  aggregation = "sum" | "min" }`; `S` is an integer, `1 ≤ S ≤ 10^9`.
- `raw` is the report's `points` attribute: grammar `[0-9]+(\.[0-9]+)?`, at
  most 18 significant digits, no exponent, no sign; it must satisfy
  `0 ≤ raw ≤ S`; anything else fails **stage 4** of §2.1 →
  `CheckerFailed{ReportInvalid}` (never clamped).
- Per-test score on a points problem: `Passed` → `S`; `Scored` → `raw`;
  `Failed` (WA/PE) → `0`, because the judge said so; an S execution failure
  decided by §6 (TLE, RE, OLE, session limit, idle) → `0`.
  `CheckerFailed`, `Unchecked`, cancelled and
  infrastructure results have **no** score.
- Aggregation in fixed point: values are `i128` in units of 10^-9 (exact for
  the grammar above up to 9 fractional digits; more fractional digits fail
  stage 4). `sum` = Σ per-test scores; `min` = the minimum. Any enabled test
  without a score makes the aggregate **`incomplete`** (with how many tests
  are missing), never a number; zero enabled tests gives **`no_tests`**, not
  a number. A scored result is final only after authority completion (§6.1).
- With `scoring = "pass_fail"`, a points result is `ProtocolUnsupported`.

## 5. Connected runs (supervisor)

### 5.1 Wiring

```
 S.stdout ──► [relay: forward, count, transcript] ──► I.stdin
 S.stdin  ◄── [relay: forward, count, transcript] ◄── I.stdout
 S.stderr, I.stderr ──► independent capture (E §5)
 I argv: per profile (§2.1)
```

Only I receives the input path (wiring, not isolation). Interactive S uses
stdin/stdout only; file-based I/O for S in an interactive problem is refused
at load (§9).

### 5.2 Modes

| | Relay (default) | Direct (`--direct`) |
|---|---|---|
| Path | S ↔ cpf ↔ I | OS pipes S ↔ I |
| Dialogue byte counts and caps | enforced | **unavailable**, reported as such |
| Transcript, idleness watchdog | available | unavailable |
| stderr capture, deadlines, kill | yes | yes |
| Timing | adds scheduling per hop; extra buffering can hide write-before-read deadlocks | closer to a judge, not equal |

This chooses the direct-mode budget semantics that E §9a left open: direct
mode explicitly gives up dialogue accounting. The report records the mode and
queue capacities; no relay overhead is ever subtracted; no PTY or
language-level unbuffering unless explicitly requested.

### 5.3 Relay per direction

Each direction is an independent state machine; no lock is shared between
directions or with transcript writing:

```
Open ──(source EOF; queue drained to dest)──► HalfClosed (close dest write end)
Open ──(dest broken pipe)──► SinkGone (stop reading; close source read end)
HalfClosed | SinkGone ──► Closed
any ──(session teardown)──► Closed (pending I/O per E §4.3 completion rule)
```

- Reads ≤ 64 KiB; each chunk is written immediately, partial writes
  continued, `EINTR` retried. In flight per direction: one chunk plus a queue
  bounded by bytes **and** records (§5.5). A blocked destination
  back-pressures only its own direction.
- EOF propagates only after every queued byte was written. EOF means "all
  writer handles closed" and can come before or after process exit.
- `SinkGone` closes the upstream read end so closure reaches the writer as a
  broken pipe; it never drains and discards.
- Transcript records go to a separate bounded queue with its own worker;
  forwarding never waits for it (§5.4).

### 5.4 Transcript

Per direction: byte offset and length of each chunk read, monotonic time
**observed by cpf**, the **forwarded prefix** of each write (bytes the
destination accepted, so a write cancelled after a prefix is recorded
exactly), EOF, half-close, write failures, final read and forwarded offsets,
process events. Bytes are stored raw. Cross-direction order is observation
order, not causal order.

- When the transcript queue or retention cap is full, records are dropped and
  **coalesced** into one gap per contiguous dropped range (byte range and
  record count); at most 64 gap markers per direction, further gaps merged
  into a final counter.
- A permanent transcript write failure sets `transcript: failed{error,
  at_offset}` in the session's in-memory run status (part of the finalized
  record, E §6.2), not only a marker in the failed file.
- A gapped or failed transcript is a diagnostic excerpt.

### 5.5 Limits

| Resource | Measurement | When | Retention | Overflow action | Attributed to |
|---|---|---|---|---|---|
| Dialogue output per direction | bytes accepted from the writer (relay) | live | — | 64 MiB: kill writer | writer: S → OLE; I → `Crashed` |
| Relay queue per direction | bytes and records | live | — | 256 KiB / 4096 records: back-pressure, no loss | — |
| Transcript | bytes and records per direction | live | 1 MiB / 65 536 records | drop + coalesce (§5.4) | — (status only) |
| stderr of S, I | E capture | live | 1 MiB each | discard beyond retention (retention only, not an execution limit) | — |
| Report | file size, bounded read | after exit | 64 KiB | `ReportInvalid` | the role that wrote it |
| `tout` | file size, bounded read | after I's cleanup | 64 MiB | `ArtefactInvalid` | I |
| Session event record (§6.1) | facts | live | ≤ 17 facts (32 reserved), ≤ 16 KiB serialized (§6.1 bound) | `internal_policy_error` (unreachable: bounded by construction) | — |

Files are checked after the writer is gone; their growth during the run is
not limited (E §5.4: no quota promise for files a program writes itself).

**Dialogue cap** (v7.1): the cap counts bytes
the relay **reads** from the writer. The relay forwards at most the cap,
detects overflow on byte `cap + 1`, and then keeps reading and **discards**
(`discarded_over_cap`) within the existing deadlines, so the destination
still sees EOF after source EOF and queue drainage. A breach observed after
the writer exited is still recorded as `StopRequested{writer,
OwnLimit(DialogueOutput)}` at its observation position (not backdated), with
no kill and `RootExited` unchanged; if the role already has a stop fact, it
is diagnostic only. Earlier decisive facts still win (§6.2), so a late
breach does not guarantee OLE; a naturally exited I keeps the ordinary
validation rules.

### 5.6 Deadlines and grace

| Phase | Default | Meaning |
|---|---|---|
| S wall deadline | problem's local limit | S's own limit; includes waiting for I and relay scheduling, without compensation (stated in the report, with S CPU, I CPU and session elapsed) |
| I wall deadline | S's deadline + `interactor_allowance_s` (1 s) | I's own limit |
| Session wall deadline | max of the two above + 1 s | the whole session |
| **Peer completion allowance** | `peer_allowance_s` = 1 s | after the first root exits, the other root's time to exit; expiry is a failure of the late role (§6.2) |
| **Final transport drain** | ≤ 500 ms | after the last root exit, queued bytes and transcript records are flushed; undelivered bytes recorded as `delivery_incomplete{direction, bytes}` (evidence, not a verdict input) |
| Termination | E §4, §5.2 | kill, cleanup, capture freeze |

**Order:** root exits → peer completion allowance → final transport
drain → descendant termination by the E kill path (group kill, cleanup
verification, capture freeze). Descendants are therefore still alive during
the drain; bytes they write then are forwarded or counted like any other.
**Loss accounting:** `delivery_incomplete.bytes` = bytes observed (read from
the source) − bytes forwarded − `discarded_over_cap` (§5.5); bytes never read from the OS pipes are
`unknown` and reported as such, never counted.

Fixtures cover both ends: a draining destination receives every byte within
the allowance; a destination that stops reading yields bounded,
explicitly incomplete delivery.

### 5.7 Idleness watchdog

Off by default. `idle_s = N` (relay only): the timer starts when both roots
are created and restarts on every **successful forwarding write** in either
direction (bytes accepted by the destination pipe, which may still sit in its
buffer). Expiry while both roots are alive stops the session with "no
successful forwarding observed for N s", never "missing flush"; the report
gives bytes read, forwarded and queued at that moment.

### 5.8 Start-up and teardown

The pair is admitted atomically (E §9a). If either creation fails, the other
is torn down (`Teardown(Infrastructure)`); the record gets a
`NotStarted{role, reason}` fact for the role that failed and an
`Infrastructure` fact (§6.1 item 8): a session that cannot be created is
`infrastructure` (E §6.4), unless the reason is `CancelledBeforeStart`. A
follow-up checker that cannot start is `NotStarted{checker}` likewise.

## 6. Verdict

### 6.1 Session event record

The session coordinator keeps an in-memory, bounded, lossless record of
**facts** (distinct from the lossy event transport, E §9). Each fact has two
positions:

- `obs_seq`: the order in which the coordinator recorded it (unique);
- `decision_pos`: the position it is **attributed** to for the verdict.
  `decision_pos = obs_seq`, except `ValidationDone` (attributed to that
  judge's `RootExited`: the judge decided when it exited, even if its report
  is read later) and `AuthorityComplete` (attributed to the `ValidationDone`
  that completed it).

**Facts** (exhaustive):

| Fact | Payload | Emitted by |
|---|---|---|
| `Started(role)` | — | supervisor |
| `NotStarted { role, reason }` | E's `NotStarted` reason | supervisor |
| `CheckerSkipped { reason }` | `NotEligible` \| `Cancelled` | initial feature release |
| `StopRequested { role, cause }` | see below | supervisor |
| `CancelRequested` | — (at most one: later requests are not facts) | supervisor |
| `RootExited { role, status, provenance }` | E `Termination`; `Natural` \| `Stopped(obs_seq)` | supervisor |
| `CleanupResult { role, cleanup }` | E §4.4 `Cleanup` | supervisor |
| `ValidationDone { role, outcome }` | see below | initial feature release |
| `AuthorityComplete { result }` | `Passed` \| `Failed` \| `Scored` | initial feature release |
| `Infrastructure { detail, suppressed }` | detail kind + text; count of later conditions (`u32`, saturating) | supervisor |

`StopRequested` records a **policy trigger**, not proof that a kill occurred
(§5.5: a late dialogue-cap breach has no kill). Stop causes: `OwnLimit(Wall | DialogueOutput)`, `PeerAllowanceExpired`,
`SessionLimit`, `Idle` (**decisive** causes), and `Teardown(PeerDecided |
Cancel | Infrastructure)` (never decisive). Memory limits are not stop causes:
an address-space limit makes allocation fail inside the program, which then
exits or dies like any other program (E §7).

`ValidationDone` outcomes: `Ok(Passed | Failed | Scored)` (only these three
`CheckResult` variants), `Failed(DeclaredFail | ReportInvalid |
ProtocolUnsupported | ArtefactInvalid)`, `Unverified`, `NotRead`. A testlib
judge's validated exit 3 / `fail` is `Failed(DeclaredFail)`.

**Emission contract** (what makes the record complete):

1. **Start.** Each role gets exactly one of `Started` / `NotStarted`; the
   follow-up checker (modes `checker`, `interactor+checker` only) gets exactly
   one of `Started` / `NotStarted` / `CheckerSkipped`. In mode `interactor` no
   checker fact exists.
2. **Stops.** At most **one** `StopRequested` per role: the first trigger
   recorded for it. Later triggers for a role already being stopped are
   diagnostics in the event transport, not facts. Simultaneous triggers are
   ordered by the same-wakeup order below, so the recorded one is
   deterministic.
3. **Teardown** stops are issued only after a decisive fact
   (`PeerDecided`), a `CancelRequested` (`Cancel`) or an `Infrastructure`
   fact (`Infrastructure`) already in the record.
4. **Exit and cleanup.** Every started role gets one `RootExited`, then one
   `CleanupResult`.
5. **Validation.** Every **started judge** (I, and the checker when
   started) gets exactly one `ValidationDone`, recorded after its
   `CleanupResult`, decided by the first rule that applies:
   1. root `Stopped` (any cause), or `Natural` with a crash status (a signal
      or abnormal termination; for `simple`, also an exit code other than
      0 and 1, §2.2) → `NotRead`; the stop or crash is the judge's decisive
      fact (or, for `Teardown`, none);
   2. cleanup `Incomplete` → `NotRead` (step 1 decides: infrastructure);
   3. cleanup `Unverified` → `Unverified`; nothing is read (§6.4);
   4. cleanup `Confirmed`: read the report (and, for I, `tout`) and run
      §2.1 stages 2–6 (§2.2 for `simple`) → `Ok(result)`, or `Failed(class)`
      including `Failed(DeclaredFail)` for a valid testlib `fail`.
      An `Ok` I result outside its authority row (§6.3) is
      `Failed(ProtocolUnsupported)`; the row check applies to `Ok` results
      only, never to `DeclaredFail`.
6. **Checker start decision.** Taken once, after S and I both have
   `CleanupResult` and I has `ValidationDone`. Eligible iff the record so
   far has no `Infrastructure`, no decisive fact (§6.2 table), no
   `CancelRequested`, and `ValidationDone{I, Ok(Passed)}`. Eligible → start
   (`Started` or `NotStarted`); otherwise `CheckerSkipped{Cancelled}` if a
   `CancelRequested` exists, else `CheckerSkipped{NotEligible}`.
7. **Authority.** `AuthorityComplete` is recorded **iff** its prerequisites
   hold, immediately after the completing `ValidationDone`:
   - mode `interactor`: `ValidationDone{I, Ok(Passed | Scored)}`;
   - modes `checker`, `interactor+checker`: `ValidationDone{I, Ok(Passed)}`
     and `ValidationDone{checker, Ok(r)}` for any `r`.
   Its result is the completing `ValidationDone`'s result.
8. **Infrastructure.** At most one fact: the first condition; later ones
   increment `suppressed`. E §6.4 infrastructure conditions visible in other
   facts (non-cancel `NotStarted`, cleanup `Incomplete`, status `Unknown`)
   are also recorded as this fact when observed.

**Bound** (by construction from the contract): per role, start 1,
`StopRequested` ≤ 1, `RootExited` ≤ 1, `CleanupResult` ≤ 1 (3 roles: ≤ 12);
`ValidationDone` ≤ 2; `AuthorityComplete` ≤ 1; `CancelRequested` ≤ 1;
`Infrastructure` ≤ 1. **Total ≤ 17.** The record reserves 32 facts.
Serialized form (the JSON in the run record, E §6.2): a fact carries only
bounded summaries of E data (the `Termination` variant with its integers and
static NTSTATUS name, the `Cleanup` variant with its count or reason tag, the
`NotStarted` reason kind with its integer codes); each role's full E
`RunRecord` is stored beside the record under E's own ≤ 64 KiB bound (E §6.1).
Every fixed field is bounded (integers ≤ 20 digits, enum tags from a fixed
list; `Scored` values in **canonical** form, rendered from the fixed-point
value of §4 without leading zeroes or trailing fractional zeroes, so
≤ 10 integer + 1 + 9 fractional = 20 characters), so a fact without text is
≤ 512 bytes; the
only text fields, the `NotStarted` reason text (e.g. an I/O error message)
and `Infrastructure.detail`, are ≤ 256
bytes **after** JSON escaping (truncated at an escape boundary, with
`truncated: true`). 17 × (512 + 256) = 13 056 bytes ≤ **16 KiB**. A test
serialises the largest instance of every variant and asserts both figures.

**Same-wakeup order** (`obs_seq` of facts delivered by one wakeup):
**repeatedly record the smallest currently eligible fact** by kind, then stop
subcause, then role; a fact is *eligible* once all its causal prerequisites
are already recorded. Prerequisites: `RootExited` after the role's `Started`
and its `StopRequested` (for `Stopped`); `CleanupResult` after `RootExited`;
contract items 3 and 5–8 (teardown after its cause, `ValidationDone` after
`CleanupResult`, the checker decision after S's and I's closure,
`AuthorityComplete` after its `ValidationDone`, `Infrastructure` from the
condition it reports); `NotStarted{CancelledBeforeStart}` after
`CancelRequested`. Kind order, exhaustive over the variants above:
`NotStarted` < `RootExited` < `CleanupResult` < `ValidationDone` <
`AuthorityComplete` < `CheckerSkipped` < `Started` < `StopRequested` by cause
(`OwnLimit(Wall)` < `OwnLimit(DialogueOutput)` < `PeerAllowanceExpired` <
`SessionLimit` < `Idle` < `Teardown(PeerDecided)` < `Teardown(Cancel)` <
`Teardown(Infrastructure)`) < `CancelRequested` < `Infrastructure`; role
order `S < I < checker`. The selection is a deterministic topological order,
so it has no cycles. Exits before timers matches E §6.3: a timer that fires
is preceded by the non-blocking exit check, so an exit already available is
ordered first.

**Closure:** the verdict is computed only on the **closed** record: every
item of the emission contract that applies has happened. Facts never change
afterwards. **Determinism:** the same closed record, authority mode and
scoring always reduce to the same verdict (different schedulings may produce
different records).

### 6.2 Reduction (policy "interactive-v7")

`reduce(record, authority, scoring) -> SessionVerdict`, a pure function over
closed records. Positions are `decision_pos`.

**Decisive facts** (with their verdicts):

| Decisive fact | Verdict |
|---|---|
| `StopRequested{S, OwnLimit(Wall)}` | TLE |
| `StopRequested{S, OwnLimit(DialogueOutput)}` | OLE |
| `StopRequested{S, PeerAllowanceExpired}` | TLE ("did not exit after the interactor finished") |
| `RootExited{S, Natural}` with nonzero exit or a signal, not normalised (§6.5) | RE |
| `StopRequested{I or checker, OwnLimit(_) or PeerAllowanceExpired}` | `CheckerFailed{Crashed}` |
| judge `RootExited{Natural}` with a crash status (rule 5.1) | `CheckerFailed{Crashed}` |
| `StopRequested{_, SessionLimit}` | `session_limit` (shown as TLE (session)) |
| `StopRequested{_, Idle}` | `idle` |
| `ValidationDone{_, Failed(class)}` | `CheckerFailed{class}` (incl. `DeclaredFail`) |
| `ValidationDone{_, Unverified}` | `Unchecked{CheckingUnverified}` |
| `ValidationDone{I, Ok(Failed{kind})}` (a valid rejection by I, any mode) | WA / PE |

Not decisive: `Teardown` stops, `ValidationDone{_, NotRead}`,
`ValidationDone{_, Ok(_)}` except I's rejection, `NotStarted{_,
CancelledBeforeStart}`, `CheckerSkipped`. A secondary I exit after a decisive
S fact is evidence only.

**Domain invariants** (checked by step 0): the emission contract (items
1–8) holds, including consistency of `CheckerSkipped`, `ValidationDone` and
`AuthorityComplete` with the facts they depend on; `Stopped(n)` refers to the
role's `StopRequested`; a `NotStarted{_, CancelledBeforeStart}` has a
`CancelRequested` before it; **decisive facts have pairwise distinct
`decision_pos`** (rule 5.1 makes a judge's crash and its `ValidationDone`
exclusive, and every other decisive fact has its own `obs_seq`), so no tie
order between decisive facts is needed; the §6.1 bound.

0. **Validate.** Any invariant violated → `internal_policy_error` (reported
   with the record; never success).
1. **Infrastructure.** An `Infrastructure` fact → `infrastructure`.
2. **Earliest decisive fact** `d` (smallest `decision_pos`).
3. **Arbitrate `d` against cancellation**: `CancelRequested` before `d`
   → `cancelled`; otherwise `d`'s verdict.
4. **No decisive fact.**
   - `CancelRequested` exists: the `AuthorityComplete` result if
     **execution complete** (every role required by the mode started, its
     root `Natural`, S with exit 0 or normalised) and `AuthorityComplete`
     both have positions before it; otherwise `cancelled` (completion of
     judging alone does not protect a still-running S).
   - Else the `AuthorityComplete` result (`Passed` → OK; `Failed` → WA/PE;
     `Scored` → score).

**Completeness** (why step 4 needs no catch-all): with no
`Infrastructure`, no decisive fact and no `CancelRequested`, the record has
no `Teardown` stop (contract item 3) and no other stop (all decisive), so
every started root is `Natural`; no role is `NotStarted` (infrastructure or
cancel); S exited 0 or normalised (else RE); each judge exited with a
non-crash status (else decisive), so its `ValidationDone` came from rule 5.3
or 5.4, and being non-decisive it is `Ok`, with I's result `Passed` or
`Scored` (I's rejection is decisive). Mode `interactor`: contract item 7
requires `AuthorityComplete`. Checker modes: I's `Scored` would be
`Failed(ProtocolUnsupported)` (§6.3), so I is `Ok(Passed)`; item 6 makes the
checker eligible, so it started; by the same argument its `ValidationDone`
is `Ok(r)`, and item 7 requires `AuthorityComplete`. Cleanups: judges
`Confirmed` (rule 5.3 is decisive), S `Confirmed` or `Unverified` (E §6.4:
`Incomplete` is infrastructure). Hence the second branch of step 4 always
has its `AuthorityComplete`. With `CancelRequested`, both branches are
total. The verdict `checking_interrupted` is unreachable: a judge stopped
by teardown always follows a decisive fact, a cancellation or
infrastructure, which decide instead.

**Coverage** (valid records):

| Event record | Decided by | Verdict |
|---|---|---|
| I hits its own deadline or dialogue cap | step 2 | `CheckerFailed{Crashed}` |
| S hits a limit; I subsequently accepts | step 2, S `OwnLimit` (earlier) | TLE/OLE |
| S hits wall and dialogue cap in one wakeup | contract item 2: `OwnLimit(Wall)` recorded | TLE |
| S exits nonzero; I then stopped by teardown | step 2, S natural crash; I `NotRead` | RE |
| S exits 0; I exceeds the peer allowance | step 2 | `CheckerFailed{Crashed}` |
| Both exit 0; follow-up checker WA | step 4, `AuthorityComplete{Failed}` | WA |
| Both exit 0; follow-up checker fails / times out | step 2 | `CheckerFailed` |
| Follow-up checker cannot be created | step 1 | `infrastructure` |
| I rejects; checker skipped (`NotEligible`) | step 2, I rejection | WA |
| `CancelRequested` before S's own-limit stop | step 3 | `cancelled` |
| Cancel before the checker starts | `CheckerSkipped{Cancelled}`, step 4 | `cancelled` |
| I scores; S independently crashes | step 2, S natural crash | RE (score kept as evidence) |
| I accepts; S dies of permitted early-close SIGPIPE | normalised (§6.5), step 4 | OK |
| Idleness fires, no S-owned limit | step 2 | `idle` |
| S times out; I then exits naturally on EOF with WA | step 2, S `OwnLimit` (earlier) | TLE |
| S crashes; I then reports WA on EOF | step 2, S natural crash | RE |
| I rejects; S then dies writing (SIGPIPE) | step 2, I rejection | WA |
| I fails to start (spawn error) | step 1 (E §6.4) | `infrastructure` |
| I crashes and its cleanup is `Unverified` | step 2, I crash (rule 5.1) | `CheckerFailed{Crashed}` |
| I exits normally, cleanup `Unverified` | step 2, `ValidationDone{I, Unverified}` | `Unchecked{CheckingUnverified}` |
| Cancel after I accepted, mode `checker`, before the checker finished | step 4, execution not complete | `cancelled` |
| Cancel after the follow-up checker's validated WA, execution complete | step 4 | WA |
| Cancel while S still runs after I accepted (mode `interactor`) | step 4, execution not complete | `cancelled` |
| I report exit 0 + `wrong-answer` | step 2, `ValidationDone{I, Failed(ReportInvalid)}` | `CheckerFailed{ReportInvalid}` |
| Follow-up checker exits 3 with a valid `fail` report | step 2, `ValidationDone{checker, Failed(DeclaredFail)}` | `CheckerFailed{DeclaredFail}` |

**Reducer tests (initial feature release):** table-driven, one test per row of the decisive
and coverage tables; every adjacent pair of the same-wakeup order;
`decision_pos` attribution of late-read reports; every rule of contract item
5; cancellation before and after `d`, before and after execution completion
and `AuthorityComplete`, in every mode; every `CheckerFailClass`; points
under pass/fail. **Property tests:** records generated from the emission
contract (all interleavings of a small model) never reach step 0's verdict
and always end in a defined verdict; mutated records violating one
invariant always stop at step 0; no success without `AuthorityComplete`; the
fact count and serialized size stay within the §6.1 bound.

### 6.3 Checking authority

`checking` is required for interactive problems (no default):

| Mode | I may | Follow-up checker | Accepts | Scores |
|---|---|---|---|---|
| `interactor` | reject, accept, score | not allowed (config error if given) | I | I |
| `checker` | reject (protocol violations); its `Passed` means "collected" | mandatory | checker | checker |
| `interactor+checker` | reject; must `Passed` | mandatory | both | checker |

An I result outside its row (a score under `checker` or
`interactor+checker`, any score under pass/fail) is
`CheckerFailed{ProtocolUnsupported}`, attributed to I's exit. "Mandatory
checker" means: it runs on every execution that is otherwise eligible and not
already decided by §6.2 step 2. The `checker` and `interactor+checker`
modes reduce identically; they differ in the author's semantic contract
(whether I's `Passed` means "collected" or "accepted").

### 6.4 `tout` and the follow-up checker

- `tout` is a fresh empty file created by cpf per session.
- Authoritative checking requires I's containment cleanup `Confirmed`
  (E §4.4), so no surviving writer can change `tout` while it is read. With
  `Unverified` the verdict is `Unchecked{CheckingUnverified}` (§6.1 rule 5.3);
  an optional diagnostic checker run is evidence, never OK.
- Missing `tout` (deleted by I) or larger than its cap →
  `CheckerFailed{ArtefactInvalid}` attributed to I.
- The follow-up checker runs through **its own profile** (§2.1:
  `checker <input> <tout> <answer> <report> -appes`; §2.2 for `simple`),
  with `tout` in the output position.
- Input, answer, `tout`, both reports and both stderr are kept for failures.

### 6.5 SIGPIPE and broken pipes

- Unix `SIGPIPE`/`EPIPE` and Windows `ERROR_BROKEN_PIPE`/`ERROR_NO_DATA`
  happen inside the child; cpf sees only the exit status. A `SIGPIPE` death
  is recognisable; an `EPIPE` turned into exit 1, or any Windows exit code,
  is not, and cpf does not pretend otherwise.
- **Normalisation:** an S death by `SIGPIPE` whose `RootExited` comes after
  I's `RootExited`, where I's result is `Passed`/`Scored` and the problem
  declares `early_reader_close = true`, is treated as exit 0. In every other
  case it is an ordinary signal (RE if decisive).
- After a valid rejection, a later SIGPIPE is secondary (step 2 ordering).
- cpf ignores `SIGPIPE` itself; children start with the default disposition
  (Rust resets it on spawn; verified by connected-run tests).

### 6.6 Display policy

Raw results (including PE) are always kept. Display profiles: `cpf`
(default; PE shown as PE) and `judge-cf` (PE shown as WA). Display never
changes the stored result.

## 7. Seeds, reruns and adaptive interactors

- **Seeds**: separate seeds per role, all recorded.
  - Generator: **always** `gen <seed>` as argv; this is the stress contract
    (design §6.5), not opt-in.
  - Interactor and solution: opt-in by environment, `CPF_SEED_INTERACTOR`,
    `CPF_SEED_SOLUTION`, each only to its role and only if that role declares
    `seed = "env"`. An argv seed is allowed only for a declared **wrapper**
    that removes it before testlib registration (stock `registerInteraction`
    rejects extra arguments). Undeclared → no seed, and the report
    says the session is not seed-controlled for that role.
- **Recorded per session:** effective argv and environment, asset and answer
  hashes, executable identities (E §2 digest), mode, limits, seeds.
- **The initial interactive release ships seeded rerun only:** run S and I again with the same seeds
  and report whether the failure reproduced. Not a determinism promise.
  Byte-stream replay against a recorded transcript is deferred: it needs its
  own spec (turn boundaries are not recoverable from read chunks); the
  transcript's forwarded prefixes and final offsets (§5.4) keep that option
  open. No rerun yields acceptance or timing equivalence.
- **Adaptive correctness** is the author's contract, documented in the
  problem's `interactor.md` template: the adversary stays within the allowed
  response space, and the acceptance rule is stated (including "the dialogue
  uniquely determines the answer" where required). cpf does not verify it;
  `cpf interactor-test` runs I against declared known-good and known-bad
  solutions (initial feature release). Passing it does not prove the invariant.

## 8. Interactive stress

- Each solution (candidate, reference) runs in its own session with the same
  generated input and interactor seed; transcripts are never compared.
- **Single reference success predicate**: the reference session's
  verdict is OK, or a score equal to the full scale `S`.
- **Outcome class per iteration**, by precedence (the first matching class):
  1. *infrastructure*: either session `infrastructure` or
     `internal_policy_error`;
  2. *cancelled*: either session `cancelled`;
  3. *generator / validator failure*: before any session;
  4. *judge failure*: either session `CheckerFailed{…}` or `Unchecked{…}`;
  5. *oracle failure*: the reference fails the success predicate (any
     S-side verdict);
  6. *candidate failure*: the candidate verdict is WA, PE, RE, TLE, OLE,
     `session_limit`, `idle`, or a score below the declared threshold;
  7. *pass*: otherwise.
  Every terminal verdict maps to exactly one class. Only candidate failures
  are reported as bugs in the solution; every class is saved with all seeds
  (design §6.5).
- Query-count and similar metrics are deferred until a typed metric schema
  exists.
- Shrinking reruns with smaller generator parameters; transcript bytes are
  never deleted to minimise.

## 9. Capability matrix

| Shape | Initial feature release | Later | Refused |
|---|---|---|---|
| Batch, checker (`testlib`, `simple`) | ✓ | | |
| Batch, scored | ✓ (`sum`, `min`) | subtasks | |
| Interactive, one session per test | ✓ | | |
| Interactive, scored | ✓ (§6.3) | | |
| File-based I/O, batch S (declared names in the private dir) | ✓ | | |
| File-based I/O for S in an interactive problem | | | ✓ |
| Output-only (submit files, no program) | | ✓ | |
| Multi-case persistent session | | ✓ | |
| Run-twice / communication | | ✓ | |
| Multi-party (manager + several S) | | | ✓ until designed |
| Grader-linked, extra sources | ✓ as `batch` with file roles (`support_files`, `solution_files`, problem `entry`; configuration contract §6.2) when the local grader is one ordinary program | callbacks per language, interface-aware stubs | |
| Byte-stream / protocol-aware replay | | ✓ | |
| Kattis validators, DMOJ checkers | | adapters (§2.4) | |

Refused shapes are rejected when the problem is loaded (design §4.1).

## 10. Configuration sketch (configuration contract owns the schema)

Batch problem with a checker:

```toml
[check]                                # problem.toml
kind = "checker"                       # "compare" (default) | "checker" | "interactive"
checker = { sources = ["checker.cpp"], language = "cpp", profile = "testlib" }
answer = "file"                        # "file" | "none"
scoring = "pass_fail"                  # | { points = true, scale = 100, aggregation = "sum" }
```

Interactive (one valid example per checking mode):

```toml
[check]
kind = "interactive"
# checker = { sources = ["checker.cpp"], language = "cpp", profile = "testlib" }  # only with "checker" / "interactor+checker"
[check.interactive]
interactor = { sources = ["interactor.cpp"], language = "cpp", profile = "testlib", seed = "env" }
checking = "interactor"                # | "checker" | "interactor+checker" (then the checker line is required)
mode = "relay"                         # | "direct"
idle_s = 0                             # 0 = watchdog off
early_reader_close = false
interactor_allowance_s = 1.0
peer_allowance_s = 1.0
```

`kind` selects exactly one of comparator, checker, interactive. A `checker`
with `checking = "interactor"` is an error; `checking = "checker"` or
`"interactor+checker"` without a `checker` is an error.

## 11. Connected-run scope and regression fixtures

The connected-run supervisor implements §5 and the execution facts of the
session event record (the facts marked supervisor in §6.1, emission contract
items 1–4 and 8, the stop coalescing and the bound); the initial feature
release adds the judge facts (`ValidationDone`, `CheckerSkipped`,
`AuthorityComplete`, items 5–7). The supervisor asserts **event and provenance
traces only**: `seq` order, stop causes, provenance, byte counts, transcript
contents, timings. It never asserts a judging verdict. Required fixtures
must pass on Linux, macOS and Windows:

1. S exceeds its wall deadline: `StopRequested{S, OwnLimit(wall)}` comes
   first; I's later exit may be `Natural` (it saw EOF) or `Stopped` (teardown):
   both are accepted, and the fixture asserts only the order.
2. I exits first; S then dies writing: I's `RootExited` precedes S's. Unix:
   S `SIGPIPE`. Windows: a platform-specific fixture where S's write fails
   with `ERROR_NO_DATA`/`ERROR_BROKEN_PIPE` and S exits with a chosen code.
3. Half-close while alive: S closes stdout and keeps running; I sees EOF,
   direction `HalfClosed`.
4. Draining destination: S writes then exits; I receives every byte within
   the peer allowance.
5. Slow destination: I stops reading; S writes more than queue + pipe
   capacity and blocks; the session deadline (set short in the fixture) ends
   it; `delivery_incomplete.bytes` = observed − forwarded exactly, unread pipe
   contents `unknown`.
6. Inherited handle: a descendant of S keeps stdout; EOF delayed, bounded by
   the peer allowance and termination.
7. Simultaneous large writes before reading: a real deadlock, ended by the
   **earliest applicable limit** (with defaults, S's wall deadline, which
   precedes the session deadline); both directions reported blocked.
8. stderr flood from S and I; forwarding unaffected.
9. Transcript queue full: forwarding continues; gaps coalesced (§5.4).
10. Permanent transcript write failure: `transcript: failed` in run status;
    forwarding unaffected.
11. Partial-write cancellation: cancel while a forward is partially written;
    the transcript records the forwarded prefix exactly.
12. Metadata saturation: 10⁵ one-byte exchanges exceed the transcript record
    cap; memory stays bounded, gaps coalesced.
13. Simultaneous exits with queued data in both directions: the order of
    §5.6 holds (allowance, then drain ≤ 500 ms, then descendant termination),
    `delivery_incomplete` recorded where the destination did not read.
14. Peer-allowance expiry, both directions: I exits and S keeps running, and
    vice versa; `StopRequested{late role, PeerAllowanceExpired}`.
15. Cancellation during a blocked forward: bounded shutdown (E §10.1).
16. Same-wakeup ordering: injected simultaneous exit and timer facts get
    `obs_seq` by the §6.1 selection rule; two triggers for one role in
    one wakeup (wall and dialogue cap) record only `OwnLimit(Wall)`.
17. Echo-interactor latency benchmark (10⁵ tiny exchanges), both modes;
    numbers recorded, no threshold.
18. Leak checks over repeated sessions (E §10.2).

## 12. Planned judging and stress features

Testlib profile and header pinning, staged validation; `simple` and validator
profiles; answers; scores; authority modes and `tout`; reducer with its tests;
display profiles; seeded rerun; `cpf interactor-test`; interactive stress.

## 13. Design caveats and rationale

PE stays PE by default (Codeforces mapping is an opt-in display profile);
the watchdog counts successful forwarding writes, not peer reads (§5.7);
S's wall deadline is not compensated for waiting on I; adaptive-interactor
invariants are not verified, and `interactor-test` is a check; multi-party
interaction is refused; byte-stream replay is deferred, and the initial
interactive release provides seeded rerun only. The reducer decides by first
decisive fact instead of inferring causality. Custom builds of prebuilt
testlib binaries are a declared-provenance restriction, not detected.

A role that cannot start is `infrastructure`, as E §6.4 requires for every
run. A separate `not_started` verdict or `CheckerFailed{Startup}` would
contradict that rule and be unreachable behind step 1. A
`checking_interrupted` verdict would also be unreachable (§6.2 completeness):
a judge stopped by teardown follows a decisive fact, cancellation or
infrastructure. Decisive facts have distinct positions, so a decision tie
order is unnecessary.
