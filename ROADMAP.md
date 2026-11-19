# Roadmap

cpforge is in early development. The following distinguishes implemented
library and test infrastructure from planned CLI workflows. Design documents
and contracts specify intended behavior, including features that are not yet
implemented. No milestone here promises a release date.

## Implemented foundation

Implemented and exercised on Linux:

- Cargo workspace, `cpf` argument/help skeleton, error and exit-code types,
  and a separate process-fixture binary with a control channel and independent
  watchdog.
- Shared pipe interface; Linux spawn, observation and reap ownership;
  signalling capability revoked before the consuming wait; process-wide
  cleanup service with bounded slots and worker credits.
- Deadlines, cancellation, process-group termination, bounded drain and
  procfs cleanup verification; bounded stdout/stderr capture and output events
  with explicit loss reporting and slow-consumer handling.
- Typed peak RSS and CPU measurements, explicit `RLIMIT_AS` installation,
  and Linux capability reporting for future diagnostics.
- Connected-run session API with atomic pair admission, independent relays,
  session and peer deadlines, dialogue caps, bounded transcripts, idle
  watchdog, direct pipes, execution facts and a latency benchmark. These
  mechanics do not derive interactive judging verdicts.

CI runs on Linux, macOS and Windows with current stable Rust, plus Rust 1.85
on Linux. On macOS and Windows it exercises the test fixture and pipe layer
only, which does not establish supervisor runtime support there.

## Process supervisor milestone

- Implement and validate macOS spawn/observe/reap ownership, cleanup
  enumeration and named measurements; offer only validated metrics and
  explicitly supported enforcement mechanisms.
- Implement Windows creation-time Job Object membership, ownership and
  inheritance rules, cancellable pipes, bounded shutdown, measurements and
  commit limits, including execution inside an existing Job Object.
- Validate limit routing end to end: configured value, units, installed
  limit, scope, setup failures and honest outcomes. Published judge limits
  must remain separate from explicit local mechanisms.
- Run the lifecycle, partial-setup failure, capture, connected-run and leak
  suites on all supported platforms. Verify numeric deadlines, bounded worker
  shutdown, disk/write failures and repeated-run resource counts.
- Record the runtime decision and evidence across platforms before treating
  the supervisor as complete.

## CLI workspace milestone

Planned:

- Configuration schema, strict validation, layered provenance, platform
  overrides, environment rules, placeholders and explicit shell execution.
- Portable identifiers and collision detection; locks, atomic replacement,
  no-clobber publication and recoverable command transactions.
- `cpf init`, `cpf add`, current-problem selection and per-problem language;
  test discovery and `cpf test add/ls/enable/disable` through the test-store
  interface. Missing and empty answers stay distinct.
- Byte-precise comparators and bounded diagnostics per the
  [comparator specification](docs/compare-spec.md).
- Fresh build outputs and artifact sets, proven with C++, Python and Java;
  failed builds must never run stale outputs.
- Job orchestration, cancellation, reports, `--json`/`--events`, byte-safe
  encoding and documented exit codes; `cpf build`, `cpf test`, raw `cpf run`,
  custom commands, repeat and opt-in trusted-workspace hooks.
- Run registration, leases, frozen inputs, pruning and retention, including
  real Ctrl-C and force-exit integration on Unix and Windows. Force exit
  needs an enumerable live-run registry because signalling capability moves
  with the cleanup lease.
- `cpf config` and `cpf doctor`, separating read-only validation from
  supervised probes and unavailable features from OS capabilities.

The [managed-workspace design](docs/design-workspace.md) adds source capture,
admission sequences, run facts and fingerprints, `cpf status`, `cpf adopt`,
`cpf ignore`, plan-first `cpf clean`, offered `.gitignore` updates and
`cpf fsck`. Its storage and configuration amendments are **settled, not yet
implemented**; their crash and conflict fixtures are part of the
implementation work.

## Initial feature release

Planned after the CLI foundation:

- Conservative AtCoder and CSES fetching, with generation-based publication,
  conflict handling and crash recovery. The interaction between editable
  tests and generations must be resolved before fetch is implemented.
- Simple and testlib checkers, validators, scores, interactive judging and
  the event-record reducer, checking authority and follow-up checker rules.
  Until supported, selecting a checker must produce an explicit unsupported
  error rather than falling back to token comparison.
- Seeded stress testing, saved failures and reruns, including adaptive
  interactors and interactor checks. Seeded reruns are not a determinism
  promise; byte-stream replay needs a separate design.
- Source-copy submission artifacts with optional `distill` integration;
  tags, status, notes and search; conservative archive snapshots, quarantine
  and restore only after their crash and conflict tests pass.

Registry/main-workspace selection, workspace moves, link-based views and
retained download originals remain sketches with unresolved blockers listed
in the managed-workspace design.

## Later versions

A TUI over the same job API; more site adapters supported by parser fixtures;
cgroup accounting and containment, explicit parallel test execution and a
sound build cache. Editor integration, hardened isolation, automatic
submission and broader test generation require separate designs.

## Known technical caveats

- Under host load,
  `supervisor_linux::cancellation_after_a_deadline_kill_and_exit_observation_is_not_a_late_trigger`
  can hit the two-second reap timeout. This known flaky test still needs a
  fix and verification under load.
- The relay target of **≤ 35 µs per round trip in release mode** is not yet
  verified. The current implementation must be measured on a quiet machine;
  overloaded-host measurements cannot establish that target.
- macOS lacks `pipe2`: `pipe()` followed by `fcntl(CLOEXEC)` leaves a process
  creation race. The macOS spawn backend **must serialise** that setup with
  process creation before runtime support is accepted.
- The test harness watchdog can target a reused process-group id if it fires
  late during a failing test; its identity handling needs further work.
- Unix processes escaping their process group are outside containment.
  Cleanup confirmation is an observation, not proof against escape; local
  execution is not a sandbox or a promise of judge-equivalent timing.
- Minimum platform versions are declared, not verified. Cross-platform CI
  and minimum-version validation remain outstanding.
