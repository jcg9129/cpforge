# Roadmap

cpforge is in early development. This file separates what is implemented from
what is planned. The design documents and contracts also describe behavior
that is not yet available. No milestone here promises a release date.

## Implemented: CLI foundation

Validated in CI on Linux and macOS:

- The process supervisor: spawn, observe and reap ownership, cancellation,
  deadlines, bounded output capture, a cleanup service and typed
  measurements.
- Connected runs for interactive problems. These are the process and relay
  mechanics only; interactive judging is not implemented yet.
- The job engine, with reports and the `--json` and `--events` outputs.
- The workspace commands: `init`, `add`, `current`, `switch`,
  `test add/ls/enable/disable`, `build`, `test`, `run`, `cmd`, `config`,
  `doctor`, `status`, `clean`, `adopt`, `ignore`, `gitignore` and `fsck`.

Limits of this version:

- On Windows the CLI builds and the commands that do not start processes
  work. `build`, `test`, `run` and `cmd` refuse with exit status 10 until a
  Windows supervisor exists.
- Selecting a checker, validator, interactor, answer or score policy is
  refused as unsupported in this version. It never falls back to plain
  comparison.

## Next: first useful release

- Fetching from AtCoder (problems and contests) and the CSES problemset,
  with polite per-host request spacing shared by every `cpf` process.
  Refetching never modifies or deletes a test file: an edited sample is
  kept and the new upstream version is stored beside it.
- Simple and testlib checkers, validators and scored problems.
- Interactive problems with adaptive interactors, and `cpf interactor-test`.
- File-based input and output for batch problems.
- Seeded stress testing, with saved failures, replay and promotion of a
  failure to a test.
- Submission artifacts: a plain source copy, or a configurable submit stage
  (for example `distill`) whose artifact is by default built and tested
  outside the workspace; an untested artifact is marked as such.
- Tags, workflow status, notes, filtered listing and search.
- Conservative archive snapshots, with trash or quarantine cleanup and
  restore.

On Windows, fetching, organisation, source-copy submission and archiving
work in full; judging, stress and the submit stage start processes and are
refused with exit status 10 until a Windows supervisor exists.

Not part of this release, and refused when requested: parallel test
execution, a build cache, cgroup accounting, subtasks, output-only and
multi-case problems, stress shrinking, byte-stream replay of interactive
sessions, per-test checker overrides, retained download originals, and
CSES contests.

A draft pre-release is planned once fetching and batch checkers work on Linux
and macOS.

## After that

- A terminal UI over the same job API.
- More site adapters as parser fixtures allow: Timus, Szkopuł, UOJ, QOJ,
  DMOJ, Library Checker, Codeforces and Project Euler.

## Later

Windows supervisor, cgroup accounting, parallel test execution, a build cache
and editor integration. Hardened isolation, automatic submission and broader
test generation would need separate designs.

## Known caveats

- Local execution is not a sandbox and does not promise judge-equivalent
  timing. Processes that leave their process group are outside containment,
  and cleanup confirmation is an observation, not proof.
- The relay target of 35 µs or less per round trip in release mode has not
  been verified on a quiet machine.
- Minimum platform versions are declared but not verified.
- Registry and main-workspace selection, workspace moves, link-based views
  and retained download originals are sketches with open questions in the
  [managed-workspace design](docs/design-workspace.md).

Evidence for the supervisor on each platform is in the
[runtime decision record](docs/decisions/runtime.md).
