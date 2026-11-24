# Submission contract

Status: draft. Normative for `cpf submit-prep`: the source-copy mode, the
generic `submit` stage with its optional version probe, the artifact test
in a directory outside the workspace, the submission versions under
`.cpforge/submit/`, their `current` reference, staleness in `cpf status`,
and their maintenance (`clean`, `fsck`, recovery, `gitignore`). It
implements design §6.7 with the narrowings of §1.2. References labelled
*S*, *E*, *C* and *R* point to `storage.md`, `execution.md`, `config.md`
and `report.md`; *sites* to `sites.md`; *workspace design* to
`../design-workspace.md`; *checkers design* to `../design-checkers.md`.
Text this contract adds to those documents is marked *(submission, draft)*
there; §15 lists it.

MUST / MUST NOT / never are requirements.

## 1. Scope and guarantees

### 1.1 What it does

- `cpf submit-prep` turns the selected problem's solution into a
  **submission**: a set of files, published as an immutable **version**
  under `.cpforge/submit/<p>/`, and makes it the problem's `current`
  submission. It prints the path of the file to submit and, for a known
  site, the submit URL; it copies the file to the clipboard on request.
- Two modes (§4.2):
  - **source copy** (§5): the effective `solution_files` (C §6.2) are
    copied. No process is started (except the clipboard helper with
    `--copy` on Linux and macOS, §9.3), nothing is built or tested; it works
    the same on Linux, macOS and Windows.
  - **stage** (§6): a configured `submit` stage (for example
    `cpp-distill`) writes the artifact; an optional version probe pins the
    tool; the artifact is then built and tested in an **external test
    directory** (§7) against the problem's tests with its selected
    checker or interactor. Linux and macOS; Windows refuses it before
    admission (§12).
- The source in `problems/<p>/` is never modified: every artifact is
  written beside it, in the reserved tree (§8).

### 1.2 What it narrows (scope changes, design ratification)

- **Source copy is untested.** Design §6.7 builds and tests the artifact
  "when the language allows" and does not exempt source copy. Here source
  copy never builds or runs anything and the manifest says `tested =
  false`. The copied files are the files `cpf test` judged, so testing them
  again would repeat `cpf test`; `cpf status` shows the solution's own
  test result next to the submission (§10).
- **Tool version.** Design §6.7 says the tool is "pinned by version". Here
  it is pinned only when the stage declares a version probe (§3.3); without
  one the tool is **unpinned**: the manifest records `version =
  "unpinned"` and the report carries the warning `tool_unpinned`.
- **Isolation of the artifact test.** Design §6.7 promises that "headers
  from the workspace cannot leak in". What this contract provides is
  narrower and is all that is promised:
  1. the artifact is built and run from a fresh directory **outside the
     workspace**, holding only the artifact and the provided files (§7.3);
  2. the artifact test's stages pass the accident guard's operand check (S §7) with
     the job's captured copies added to the rejected area (§6.6).
  It does **not** detect an absolute `#include` (or an absolute import or
  file open) naming a workspace file, an include path in an inherited
  environment variable (`CPATH`, `CPLUS_INCLUDE_PATH`, `PYTHONPATH`), a
  wrapper program, or a shell script's text: the guard inspects operands
  only (S §7 rule 3). An artifact that reads a workspace file through such
  a path builds and passes; §14.5 SB-S12 asserts this documented limit.

### 1.3 Guarantee levels

The storage levels of S §1 apply. Process crash: §13 lists every state a
kill can leave and what recovery makes of it; no state names a partial
version as `current`. Power loss: best effort (S §4).

## 2. Terms

- **Effective solution files**: the problem's `solution_files` after the
  C §6.2 default (`sources` followed by `support_files` when the key is
  absent). An explicitly empty list is valid configuration and means
  "nothing to submit" here (§5.1). No further fallback exists.
- **Captured files**: `sources` followed by `support_files` (C §6.2).
- **Solution sources**: the members of `sources` that are also effective
  solution files, in `sources` order.
- **Provided files**: the captured files that are not effective solution
  files, in captured order (for a grader task: the grader and its header;
  with the default `solution_files`: none). The judge supplies them, so the
  artifact test supplies them too (§7.3).
- **Primary solution source**: the single solution source when there is
  exactly one; otherwise none.
- **Artifact set**: the files a mode produces (§5.2, §6.4), each with a
  relative path, SHA-256 and size.
- **Primary file**: the file the user submits (§6.4 for stage mode; in
  source copy, the single solution file when there is exactly one).
- **Version**: one published, immutable directory
  `.cpforge/submit/<p>/<vid>/` (§8.1).
- **`current`**: the record `.cpforge/submit/<p>/current` naming one
  version (§8.3).
- **Referenced version**: the version `current` names; every other
  version of the problem is **unreferenced**.
- **External test directory** (`D`): the directory the artifact test
  builds and runs from (§7).
- `<p>` is the problem's directory name under `problems/` (S §2.1:
  `local` problems by name, remote ones `<site>-<native>`).

## 3. Configuration

### 3.1 Keys

| Key | Type | Layers | Notes |
|---|---|---|---|
| `languages.<l>.submit` | submit table | 1, 2 | §3.2; a platform overlay site like `languages.<l>.run` (C §2) |
| `submit.clipboard` | boolean | 1, 2 | default `false`; §9.3 |
| `submit.test_dir` | config-relative path (C §4.2) | 1, 2 | base of the external test directory; default the OS temporary directory; §7.1 |

### 3.2 The submit table

The stage fields of C §4.1 (`argv` or `shell`, `cwd`, `env`, `env_remove`,
`deadline_s`, `stdout_cap`, `stderr_cap`, `limits`) plus:

| Field | Type | Default | Meaning |
|---|---|---|---|
| `version` | table `{ argv, expect }` | absent (unpinned) | §3.3 |
| `test` | boolean | `true` | `false`: the artifact is never built or tested (`tested = false`); for tools whose output the target's stages cannot build |
| `judge` | string, a name (C §1.1 names) | the site name of a remote problem | the value of `{judge}` (§3.4) |

- Merge: C §2 rules; the table merges key by key across layers and
  overlays, the stage command (`argv`/`shell`) and `version` are
  replace-as-unit.
- Phase 2: a present submit table needs exactly one complete `argv` or
  `shell` (else "submit stage has no command"). A table that is
  overlay-only and inactive on the platform is absent (C §2).
- Defaults when unset: `deadline_s` 60 s; stdout and stderr caps as for a
  build (E §5.5 *(submission, draft)*).
- The problem layer has no `config.submit`: a submission tool is a
  language setting (design §6.7). `config.submit` is a phase-1 unknown key.

### 3.3 Version probe

`version = { argv = ["distill", "--version"], expect = "^distill 1\\.4\\." }`.

- `argv`: a non-empty array of strings with **no placeholders** (a `{` or
  `}` is a phase-1 error; it identifies a tool, it consumes no input).
  `argv[0]` is resolved by C §4.4 against the submit stage's environment
  (its `env`, `env_remove` applied, C §4.3), in phase 3. It is not
  required to resolve to the stage's program; both resolved paths are
  recorded.
- `expect`: a regular expression in the syntax of the `regex` crate
  (a new dependency, pinned to an exact version that supports the MSRV 1.85), compiled in
  phase 1 with a size limit of 64 KiB (`RegexBuilder::size_limit`); an
  invalid or oversized pattern is a phase-1 error. It is **searched**
  (unanchored unless the pattern anchors itself) in the version line.
- **Version line**: the probe's stdout up to the first LF, a trailing CR
  removed; it MUST be UTF-8 and at most 256 bytes. The probe runs with
  stdin null, stdout captured with a 64 KiB budget (`Kill`), stderr 64 KiB
  (`Discard`), cwd its own fresh `cwd/`, wall deadline 10 s (*fixed*).
- Outcomes (§6.3): class `exited_zero` (R §2.2) and the line matches →
  **pinned**, the line recorded; class `exited_zero` and no line, a line that is not
  UTF-8, longer than 256 bytes, or that does not match → **mismatch**
  (verdict `submit_version_mismatch`, exit 7); any other class → the probe
  failed (verdict `submit_failed`, exit 3; infrastructure → 10).

### 3.4 Placeholders of the submit stage

The C §5.2 table gains these rows *(submission, draft)*:

| Placeholder | Kind | Available when |
|---|---|---|
| `{submit_dir}` | scalar | submit stage: the job's empty `submit/out/` directory (§6.4) |
| `{submit_file}` | scalar | submit stage, when the target has a primary solution source: `{submit_dir}` joined with that source's problem-relative path; its parent directories exist before the stage starts |
| `{solution_sources}` | multi | submit stage: the captured copies of the solution sources (whole element only) |
| `{judge}` | scalar | submit stage: `--judge`, else the table's `judge`, else the site name of a remote problem (`atcoder`, `cses`) |

- Also available in the submit stage: `{sources}`, `{source}` (the captured
  copies, S §7), `{entry}`, `{problem_id}`. Not available:
  `{problem_dir}`, `{workspace}` (phase-2 error, the accident guard rule
  1), `{artifact}`, `{artifact_dir}` (no build precedes the stage),
  `{test_input}`, `{test_answer}`, `{iteration}`.
- `{submit_file}` without a primary solution source, and `{judge}` with
  no value (a local problem without `--judge` or `judge`), are phase-3
  errors naming the placeholder and the reason.
- `{sources}` keeps its meaning (C §6.2: every tool input). For a grader
  task the tool usually needs `{solution_sources}`, so that only the
  contestant's file is processed.
- The submit stage is a **source-consuming stage** (S §7): the accident
  guard applies to it and its capture check is recorded (`checked`, or
  `unchecked{shell_stage}`).

### 3.5 Validation phases

| Phase | Submission checks |
|---|---|
| 1 raw | keys and types of §3.1–3.3, placeholder grammar, the `expect` pattern, `version.argv` without braces, `submit.test_dir` path form |
| 2 effective | a present submit table has one complete command; placeholder declarations (§3.4); the effective solution files are a subset of the captured files (C §6.2, unchanged) |
| 3 selected plan (`submit-prep`) | source copy: each effective solution file exists as a regular file without a link component, outside the reserved trees (C §6.2 phase-3 rules). Stage: as `cpf test` for the artifact test's stages (§6.6), plus the submit stage's and probe's program resolution, `{submit_file}` and `{judge}` availability, the accident guard on the submit stage and on the artifact test's source-consuming stage (§6.6), the test-directory base (§7.1), the run count and caps (§6.2) |

`cpf config show` prints the submit table with provenance and the
effective mode (`source_copy` or `stage`) for the selected problem;
`cpf config check` lists the submit stage's and the probe's programs with
the tools of the other stages.

## 4. Command

### 4.1 Synopsis

```
cpf submit-prep [--problem <p>] [--source-copy] [--no-test]
                [--profile <name>] [--judge <name>]
                [--copy | --no-copy] [--json | --events]
```

- `--source-copy` selects source copy even when a submit stage exists.
- `--no-test`: stage mode without the artifact test (`tested = false`).
  With `--source-copy` it is accepted and changes nothing.
- `--profile` selects the profile of the artifact test's build and run
  stages (C §6.5); with source copy it is a usage error (2).
- `--judge` sets `{judge}`; with source copy it is a usage error (2).
- `--copy` / `--no-copy` override `submit.clipboard` (clipboard, §9.3).
- `--events`: stage mode only; with source copy (selected or forced) it is
  a usage error (2), decided once the mode is known (§4.3 step 3), before
  the lock.

### 4.2 Mode selection

Stage mode iff `--source-copy` is absent and the target's language has an
effective submit table on the validation platform (§3.2); otherwise source
copy. The mode is decided after configuration phases 1–2 and before any
other check. `cpf config show` reports the same decision.

### 4.3 Order of checks

1. Arguments (clap, then the combinations of §4.1): usage, 2.
2. Workspace and problem selection (S §5.7): 8.
3. Configuration phases 1–2: 7. Then the mode (§4.2), and `--events`
   with source copy: 2.
4. Problem kind (§4.4) and the early refusals (the judging refusals of
   C §9 "unsupported in this version", while they apply; §6.2): 7.
5. Stage mode on Windows: refused, 10 (§12). Nothing has been locked,
   created or read beyond configuration and the problem record.
6. Phase 3 (§3.5): 7.
7. Source copy: the transaction of §5.3. Stage mode: job admission (S §6.2)
   and §6.

### 4.4 Problem kinds

| Effective kind (C §9) | Source copy | Stage mode |
|---|---|---|
| `batch` | ✓ | ✓; with a `[check]` role the artifact test is supported exactly when `cpf test` supports the problem's `[check]` (profiles `simple`, `testlib` once implemented) |
| `interactive` | ✓ | ✓ once interactive judging is implemented (the artifact test runs sessions with the interactor); until then refused like `cpf test` |
| `answer`, `multi-file`, unknown | refused, 7, "unsupported in this version" | refused, 7 |

With `--no-test` or `test = false` the artifact test is not planned, so
none of the judging refusals apply; the kind row still does.

## 5. Source-copy mode

### 5.1 Input

The effective solution files (§2). An empty list → exit 7, "nothing to
submit: `solution_files` is empty", before the lock; nothing is created.
Bounds: at most 256 files and 64 MiB in total, enforced while copying
(sizes can change after phase 3); exceeding either aborts the transaction
(§5.3 step 3) with exit 7 "submission too large".

### 5.2 Artifact set

Each effective solution file at its problem-relative path, in configured
order. The artifact hashes equal the input hashes (the copy is the
artifact). Primary file: the single solution file when there is exactly
one; with several, none (the version's `files/` directory is printed and
the clipboard is skipped, §9).

### 5.3 Transaction

Under the workspace lock, after leftover recovery (S §4.5):

0. Classify `.cpforge/submit/<p>/current` (S §9): `too_new` → exit 8
   "needs a newer cpf"; `unreadable` → exit 8 naming the error; nothing is
   created. `missing`, `malformed` (replaced at step 6) and `valid` proceed.
   The lock is held from here to step 6, so the classification stays
   true until the replace.
1. Choose `<vid>` (§8.2). Create `.cpforge/staging/submit-<vid>/` and its
   `files/` (S §3: staging only under the lock).
2. Copy each file (regular files only, opened without following links;
   exclusive creation of each destination, parents created as needed),
   computing SHA-256 and size while copying; `sync_all` each copy.
3. On a read error, a source that is no longer a regular file or the
   bound of §5.1: remove the staging directory, release the lock, exit 8
   (7 for the bound); nothing published.
4. Render `manifest.toml` (§8.4); more than 1 MiB → as step 3 with exit 7
   "submission manifest too large". Write it by exclusive creation and `sync_all`;
   sync the staged directories bottom-up (S §4.3).
5. Create `.cpforge/submit/<p>/` if absent (through the guard's checked
   paths, S §2). `publish_dir_noclobber` the staging directory to
   `.cpforge/submit/<p>/<vid>` (S §4.3). **Commit point 1** (the version
   exists). `Exists` → choose a new `<vid>`, replace the staged manifest
   (its `id`) by atomic replace inside the staging directory and publish
   again, at most 3 attempts in total, then exit 8. Any other failure:
   staging left for recovery, exit 8, nothing published.
6. Atomic replace (S §4.2) of `.cpforge/submit/<p>/current` (§8.3) naming
   `<vid>`. **Commit point 2.** Failure (not committed): exit 9, the
   version stays unreferenced (pruned by `clean`, §11.1), `current`
   unchanged.
7. Release the lock. Durability and cleanup results of steps 5–6 after
   commit are warnings (S §4.1). Then the clipboard (§9.3) and the output
   (§9.1).

The lock is held from step 0 to step 6 only; no step starts a process.

### 5.4 Outcomes

| Exit | Condition |
|---|---|
| 0 | both commit points reached (warnings allowed: durability, clipboard) |
| 2 | §4.1 combinations |
| 7 | configuration phases 1–3, empty list, bound, problem kind |
| 8 | workspace selection, lock `Busy`, read or write error, not committed at step 5 |
| 9 | committed at step 5, not at step 6 |
| 11 | `--json` delivery failed |

## 6. Stage mode: the `submit` job

### 6.1 Job

A job of kind `submit` (R §1 *(submission, draft)*), with the ordinary
admission, freeze and capture of S §§6.2, 7: tests are frozen (unless the
artifact test is not planned), the captured files are copied into
`inputs/src/`. Its report is a `job_report` with `command: "submit_prep"`
and the member `submission` (§9.2). It never publishes a run fact (R §4.9:
`fact` is `null`) and runs no `after_test` hooks: the artifact is not the
solution target.

### 6.2 Plan

Planned executions, in plan order:

1. the version probe, if declared (stage `submit_probe`);
2. the submit stage (stage `submit`);
3. if the artifact test is planned (`test = true` and no `--no-test`):
   the **artifact build** (the target's build stage, if it has one, as the
   job's `build`), the role builds (C §8.2 order: checker, validator,
   interactor), then per selected test the runs of a `cpf test` plan
   (C §8.2: validator, solution, checker; sessions for interactive).

The selected tests are those `cpf test` selects without arguments (the
enabled tests). The run cap of 10 000, the judged-test cap of 1 000 and
the report reservation cap of C §8.2 apply to this plan, with the report
bound of §9.2. These are phase-3 errors, as for `cpf test`.

The judging refusals of C §9 ("unsupported in this version") apply to the
artifact test exactly as to `cpf test`, for as long as they apply there,
checked at §4.3 step 4.

### 6.3 Probe and submit stage

- The probe runs first; no other execution is admitted before its outcome
  is known. Mismatch or failure: every later planned execution is
  `skipped{submit_not_succeeded}` (R §2.1 *(submission, draft)*; after
  cancellation intent `skipped{cancelled}` takes priority, R §2.1 order); no
  external test directory is created.
- The submit stage runs with its fresh `cwd/` (the default; a configured
  `cwd` is allowed, subject to the accident guard), stdin null,
  `{submit_dir}` = `<job>/submit/out/` created empty before admission of
  the run, and the parents of `{submit_file}` created inside it.
- Class `exited_zero` → the selection rule (§6.4). `infrastructure` or
  `cancelled` (also a `NotStarted` setup failure of the probe or the stage)
  → job verdict `infrastructure` or `cancelled` (R §2.7 rows 2–3 count both
  executions). `wall_deadline`, `output_budget`, `signal`, `nonzero_exit` →
  verdict `submit_failed`. Later executions are skipped as above.

### 6.4 Artifact selection rule

After the submit stage's class `exited_zero`, `<job>/submit/out/` is walked without
following links (S §2 walker):

| Check | Failure reason (`artifact_invalid`) |
|---|---|
| only directories and regular files (no link, no special file) | `not_regular` |
| every relative path a sequence of portable names (S §2.1), UTF-8 | `name` |
| 1 ≤ files ≤ 256 | `empty`, `too_many` |
| total size ≤ 64 MiB (counted in the job budget, S §6.4) | `too_large` |
| if the stage's command uses `{submit_file}`: that path is a regular file | `submit_file_missing` |
| if the artifact test is planned: no artifact path equals a provided file's path (compared as C §6.2 duplicates) | `provided_collision` |
| if the artifact test is planned: every member of `sources` is either a provided file or an artifact path | `source_missing` (naming the path) |
| the manifest of this artifact (§8.4), rendered with the job's values, is ≤ 1 MiB | `manifest_too_large` |

- A failed check → verdict `submit_failed`, `submission.preparation` =
  `artifact_invalid{reason, path}` (§9.2; `path` the first offending
  relative path when there is one), later executions skipped, the job's
  `submit/out/` kept for inspection (`submission.path` names it).
- Each file is hashed (SHA-256, size) once, here; these are the artifact
  hashes of the manifest.
- **Primary file**: `{submit_file}`'s path when the stage uses it; else the
  single file when there is exactly one; else none.
- A tool whose output does not satisfy the `provided_collision` or
  `source_missing` rows (e.g. it renames the source) needs `test = false`;
  `test = false` never bypasses `manifest_too_large` or any other row.

### 6.5 Artifact test

Runs only when planned and the artifact was selected. Its layout and
lifecycle are §7.

- The **artifact build** is the target's effective build stage (with
  `--profile`), with `{sources}`/`{source}` expanding to the paths in `D`
  of `sources` (in configured order) and `{artifact}`/`{artifact_dir}` to a
  fresh output root in the job (S §6.1 `builds/<rid>/`). A build-free
  target has none; its run stage's `{sources}`/`{source}` name the paths
  in `D`.
- Role builds, judge runs and sessions are exactly those of `cpf test`
  (C §9.1, E §2.1): role sources come from the job's role capture, judges
  run in their private directories in the job.
- Test runs are the target's run stage with `{artifact}` /
  `{artifact_dir}` from the artifact build. Every run's cwd is its own
  fresh `cwd/` in the job (S §7).
- Verdicts per test and for the job follow R §§2.4–2.7, 2.11–2.13.

### 6.6 Guard of the artifact test

Phase 3, before admission (`D` is chosen at plan time, §7.2, so its path is
known):

- The artifact build stage and the run stage MUST use `cwd = { selector =
  "run" }` (the default); any other `cwd` → phase-3 error "the artifact
  test needs the run's own working directory".
- The accident guard (S §7 rules 1–3) is applied to the artifact test's
  source-consuming stage (its build stage, or the run stage of a
  build-free target) with one change: a path operand inside the job's
  `inputs/` directory is rejected too (the captured copies are workspace
  content). Paths in `D`, in the job's build output root and `cwd/`, and
  outside the workspace are accepted. The result is the submission's
  `test_check` (§9.2): `checked` or `unchecked{shell_stage}`.

### 6.7 Verdict, publication, exit

- `primary_verdict` (R §2.7 *(submission, draft)*): rows 1–3 as for every
  job; then `submit_version_mismatch` (the probe mismatched); then
  `submit_failed` (probe or submit stage failed, or `artifact_invalid`);
  then the `test`-job rows (`build_failed` for the artifact build or a
  role build, …, `no_tests`, `scored`, `passed`). A plan without the
  artifact test that reached a selected artifact is `succeeded`.
- **Publication** happens at phase A (R §2.9 *(submission, draft)*), in
  place of fact publication. **Eligible** iff `primary_verdict` is
  `passed`, `scored` with a full aggregate, or `succeeded`; otherwise
  `not_attempted{not_eligible}`. `no_tests` and `unchecked` are not
  eligible (an artifact that was not judged on every test is not reported
  as tested; `--no-test` publishes it untested).
- An eligible job, outside the workspace lock: moves `<job>/submit/out/` to
  `<job>/submit/version/files/` (rename inside the job), writes
  `<job>/submit/version/manifest.toml` (§8.4; `tested = true` iff the
  artifact test was planned), syncs the files and directories bottom-up.
  Then, under the workspace lock, the **cancellation cutoff** (one read of
  intent under the verdict lock; set → `not_attempted{cancelled}`, nothing
  published), then §5.3 step 0 (`too_new` or `unreadable` `current` →
  `not_published{current_too_new | current_unreadable}`, nothing
  published, exit 8), then §5.3 steps 5–6 with `<job>/submit/version` as the
  source directory (same filesystem: both under `.cpforge/`; `Exists` →
  new `<vid>`, the manifest rewritten in the job, at most 3 attempts).
  Phase 3 classifies `current` too, so a `too_new` one refuses the job
  before admission (exit 8, nothing run); phase A repeats it because the
  record can change meanwhile.
  Intent set after the cutoff never prevents or undoes the publication.
- Outcomes (§9.2 `publication`) and their exit effect (R §5.2
  *(submission, draft)*): `published{current: true}` none;
  `published{current: false}` (step 6 not committed) → failure class
  `partial`, 9; `not_published` (step 5 not committed, lock `Busy`, I/O) →
  class `workspace`, 8; `not_attempted` none (the verdict decides).
- Exit statuses of `cpf submit-prep` in stage mode: 0, 1, 3, 4, 5, 7, 8,
  9, 10, 11, cancellation (R §5.5 *(submission, draft)*).

## 7. External test directory

### 7.1 Base

- The base `B` is `submit.test_dir`, else the OS temporary directory
  (Unix `$TMPDIR` if non-empty and absolute, else `/tmp`; macOS the same
  rule, which normally gives the per-user `/var/folders/…` directory).
- Phase 3: `B` MUST exist and be a directory; it is canonicalised once
  (links allowed, as for the workspace root, S §2); the canonical `B` MUST
  NOT be the canonical workspace root or inside it (component-wise, ASCII
  case-insensitive on macOS). Otherwise phase-3 error "the test directory
  base must be outside the workspace" (or "… does not exist").
- `D` is a new, empty directory created by cpf, so it is never the
  workspace, inside it, or an ancestor of it.

### 7.2 Name and record

- `D = B/cpforge-submit-<r>` with `<r>` 32 lowercase hex digits (128
  random bits), chosen at plan time (§6.6).
- The **external record** `<job>/submit/external.toml` (tool-only, S §9
  classification, schema 1): `schema = 1`, `base` (canonical `B` as a
  lossless native string, §8.4 encoding), `name`, `token` (32 hex digits,
  random, distinct from `<r>`), `job` (the job id).

### 7.3 Lifecycle

Written by the job under its lease, without the workspace lock (the record
and `D` are the job's own):

1. **Record first**: write the external record by exclusive creation and
   `sync_all`, sync `<job>/submit/`. Crash point `submit.external_recorded`.
2. `create_dir(D)` (Unix mode 0700). An existing `D` → infrastructure
   (`NotStarted{Setup}` on the artifact build, verdict `infrastructure`);
   cpf never uses a directory it did not create. Crash point
   `submit.external_created`.
3. Create the **marker** `D/.cpforge-submit` by exclusive creation, holding
   the token and the job id, `sync_all`. Crash point
   `submit.external_marked`.
4. Copy in, exactly: the **provided files** from the job's captured copies
   (`inputs/src/`), at their problem-relative paths; the **artifact set**
   from `<job>/submit/out/`, at its relative paths. Regular files only,
   exclusive creation, parents created; each copy's SHA-256 compared with
   the capture or artifact hash (a difference is infrastructure). Bytes are
   reserved in the job budget first (S §6.4). Nothing else is placed in
   `D`: not the solution files that the artifact replaces, not role
   binaries (judges run in their private directories in the job, §6.5),
   not job inputs, records or persistence, which stay in the job directory
   (S §7). Crash point `submit.external_filled`.
5. The artifact test (§6.5).
6. **Removal**, after every execution of the job has its record and its
   cleanup result, and before phase V: verify that `D` is a directory (not
   a link) whose marker holds the token; delete `D` with the no-follow
   walker (S §2); then unlink the external record. Crash point
   `submit.external_removed` (after the walker, before the unlink). A
   failed verification or deletion leaves `D` and the record in place and
   is the warning `submit_test_dir` (`detail: leftover`; the path is in
   `submission.external_dir`); the verdict is unchanged.

A step-1 to step-4 failure is infrastructure (`NotStarted{Setup}` on the
first artifact-test execution, later ones skipped); removal (step 6) still
runs for whatever exists.

### 7.4 Recovery

The workspace is never renamed or hidden for the test (that would hide
cpf's own job infrastructure, S §7); recovery of `D` goes through its
owning job directory:

- **Pruning** (S §6.3 step 3 *(submission, draft)*): before the no-follow
  deletion of `.reclaim-<job>`, if `submit/external.toml` exists in it,
  classify it (S §9):
  - `valid`: let `D = base/name`. Every component of `base` and `D` is
    checked with `symlink_metadata` (S §2): a link or a non-directory
    anywhere → kept, reported. `D` absent → nothing. `D` is deleted with
    the walker **only** when its marker is proven: `.cpforge-submit` opened
    without following links, a regular file (checked on the handle), at
    most 256 bytes, holding exactly the record's token and job id.
    Anything else (no marker, a link, another type, other content) → `D`
    kept and reported (`test_dir_kept`): it is not provably cpf's. A crash
    between `create_dir` and the marker (§7.3 step 2–3) therefore leaks one
    empty directory in `B`, documented and left to the OS cleaner.
  - `unreadable`: the job directory is **not** pruned (it is the only
    pointer to `D`); reported, retried next time. Classified in step 1, so
    the directory is never renamed to `.reclaim-*` (S §6.3 step 1 already
    keeps directories with unreadable records).
  - `malformed`: pruned; reported (`test_dir_lost`): `D`, if it exists, is
    left to the OS temporary-directory cleaner.
  - `too_new`: the job directory is never pruned (S §9 newer schema).
- Pruning findings are `prune` warnings (R §4.10, details `test_dir_kept`,
  `test_dir_lost`) at admission and `clean` findings in `clean`.
- A live job's lease is held, so its directory and `D` are never
  candidates (S §6.3 step 2). `cpf clean --apply` reaches every inactive
  job (zero retention budget) and so every leftover `D` of the workspace.
- `cpf fsck` reports every job directory whose external record is present
  and the state of its `D` (absent, present and marked, unmarked, foreign);
  `--apply` never removes `D` (only pruning does).
- Directories under `B` whose job directory no longer exists (a deleted
  workspace) are not found by cpf; they are documented as left to the OS.

## 8. Versions and `current`

### 8.1 Layout

```
.cpforge/submit/<p>/
    current                         reference (§8.3)
    <vid>/                          one version, immutable
        manifest.toml               §8.4
        files/<relative paths>      the artifact set
    .reclaim-<vid>/                 being deleted (§11.1)
```

Nothing under `<vid>/` is modified after publication. Readers verify a
file against the manifest hash before relying on it (the clipboard,
§9.3, and the archive contract do).

### 8.2 Version id

`<vid>` = `v-<UTC yyyymmddThhmmssfffZ>-<8 lowercase hex random>`, e.g.
`v-20261124T153012345Z-1a2b3c4d`: system clock at step 1 of §5.3 (or of
the phase-A publication), 32 random bits. It does not depend on the job
sequence, so `fsck` sequence repair (a new epoch, S §5.6) cannot collide
with it. Grammar: exactly that form; anything else in `submit/<p>/` is an
unexpected entry (§11.2). Uniqueness comes from no-clobber publication
(§5.3 step 5 retries), not from the clock.

### 8.3 `current`

TOML, tool-only (S §9 record classification), schema 1:

```toml
schema = 1
version = "v-20261124T153012345Z-1a2b3c4d"
```

Written only by atomic replace (S §4.2) under the workspace lock, and only
right after the version it names was published by the same operation. It
is never rewritten to an older version and never removed by cpf (archive
retirement moves the whole tree, archive contract). **Dangling**: valid,
but its version directory is absent or its manifest is not `valid`.

### 8.4 Manifest

`manifest.toml`, tool-only, schema 1, unknown keys `malformed`, ≤ 1 MiB
(S §9). The file counts do not bound its size (long relative paths do not
fit 256 entries under the cap), so the exact rendered bytes are checked
before publication: source copy refuses at §5.3 step 4 with exit 7
"submission manifest too large", nothing published; stage mode at the
selection rule (§6.4 `manifest_too_large`). The phase-A rewrite only fills
fields of fixed maximum size reserved in that check (`id`, `[test]`).

| Key | Type | Meaning |
|---|---|---|
| `schema` | integer | 1 |
| `id` | string | the `<vid>` (MUST equal the directory name) |
| `problem` | string | the problem identity |
| `created_at` | string | report `time` format |
| `mode` | string | `source_copy`, `stage` |
| `tested` | boolean | `true` iff the artifact test ran and the job was eligible (§6.7) |
| `language`, `profile` | string; string? | the target's language; the artifact test's profile when one was selected |
| `job` | string? | stage mode: the job id |
| `judge` | string? | stage mode: the value of `{judge}` when used |
| `[tool]` | table? | stage mode: `program` (lossless native, below), `spec_sha256` (the submit stage's `spec_digest`), `version` (the version line, or the string `"unpinned"`), `probe_program` (lossless native, absent when unpinned) |
| `[[input]]` | array | the staleness basis (§10): `path` (problem-relative, `/`-joined), `sha256`, `bytes`. Source copy: the effective solution files; stage: the captured files (hashes of the captured copies) |
| `[[solution]]` | array | stage mode: the effective solution files in order (`path`), the basis of `{solution_sources}` and of the provided files |
| `[[file]]` | array | the artifact set: `path` (relative to `files/`), `sha256`, `bytes`, in artifact order |
| `primary` | string? | the primary file's path, absent when none |
| `[test]` | table? | stage with the artifact test: `verdict` (the job verdict token), `tests` (count), `score` (R `score` string, when points) |

Lossless native strings (paths that may not be UTF-8): a table `{ text =
"…" }` when the value is valid Unicode, else `{ bytes_b64 = "…" }` (Unix
raw bytes, Windows WTF-16LE), as R §3.3 does for JSON.

## 9. Output

### 9.1 Human output

On stderr (R §8): `submission <vid> (<mode>, tested|untested)`, `path:
<absolute path>` (the primary file, else the version's `files/`
directory), `submit: <url>` when known (§9.4), then warnings. Paths are
native, Windows without the verbatim prefix (S §2).

### 9.2 Machine output

Common type `Submission` *(submission, draft)*:

| Member | Type | Meaning |
|---|---|---|
| `mode` | token | `source_copy`, `stage` |
| `id` | `string?` | the `<vid>`; `null` when nothing was published |
| `publication` | object | `{"kind":"published","current":bool}`, `{"kind":"not_published","reason":token,"message":text?}` (`lock_busy`, `io`, `exists`, `current_too_new`, `current_unreadable`), `{"kind":"not_attempted","reason":token}` (`not_eligible`, `cancelled`) |
| `preparation` | object | `{"kind":"selected"}`, `{"kind":"artifact_invalid","reason":token,"path":nsum(128)?}` (§6.4 reasons), `{"kind":"not_reached"}` (probe or stage did not reach `exited_zero`); source copy: `selected` |
| `path` | `nsum(1024)?` | where the artifact is now: published → as §9.1; phase-A staging done but not published → `<job>/submit/version/files/`; otherwise an artifact in the job → `<job>/submit/out/`; no artifact → `null`. The path named exists when the report is written |
| `primary` | `nsum(128)?` | the primary file's relative path |
| `files` | `[{path: nsum(128), sha256: hex64, bytes: u64}; 16]` | the first 16 artifact files |
| `files_count`, `bytes` | `u32`, `u64` | totals |
| `inputs_sha256` | `hex64` | SHA-256 over the `[[input]]` list in workspace design §3.2 encoding |
| `tested` | bool | as the manifest |
| `tool_version` | object? | `{"kind":"pinned","line":nsum(256)}`, `{"kind":"mismatch","line":nsum(256)?}` (`null` line: none, not UTF-8 or too long), `{"kind":"unpinned"}`; `null` in source copy and when the probe failed |
| `judge` | `ident?` | |
| `submit_url` | `nsum(512)?` | §9.4 |
| `clipboard` | object | `{"kind":"not_requested"}`, `{"kind":"copied"}`, `{"kind":"skipped","reason":token}` (`no_primary`, `too_large`, `not_text`, `not_published`, `cancelled`), `{"kind":"unavailable","reason":token}` (`no_display`, `tool_missing`), `{"kind":"failed","reason":token,"message":text?}` (`exit`, `timeout`, `helper`, `io`, `win32`) |
| `test_check` | `Check?` | §6.6; `null` without the artifact test |
| `external_dir` | object? | `{path: nsum(1024), state: token}` (`removed`, `leftover`); `null` when no `D` was created |

Serialized bound 16 942 B ≤ **17 KiB** (17 408 B; the 16 file entries
of 521 B dominate); the final schema reconciliation adds it to the report header
and re-derives the reservation.

- **Source copy**, `--json`: the document `{"schema":1,"document":
  "submit","platform":…,"problem":pident,"submission":Submission,
  "warnings":[Warning; 24]}`; not persisted. A failure writes the R §4.12
  `error` document with `command: "submit_prep"`.
- **Stage mode**: the `job_report` member `submission: Submission?`
  (`null` for other job kinds; for a refusal `null`), after `fact`.

### 9.3 Clipboard

Requested by `--copy`, or `submit.clipboard = true` without
`--no-copy`. Attempted only after commit point 1 (§5.3 step 5), after
the lock is released, and only for a primary file of at most 4 MiB, whose
bytes are first re-read and checked against the manifest hash (a
difference → `failed{io}`). A failure is a warning (`clipboard`) and never
changes the exit status.

| OS | Mechanism |
|---|---|
| macOS | `pbcopy` |
| Linux | `WAYLAND_DISPLAY` non-empty → `wl-copy`; else `DISPLAY` non-empty → `xclip -selection clipboard`; else `unavailable{no_display}` |
| Windows | Win32 clipboard API through `windows-sys`, no process |

- **Unix helpers** run as an E **helper operation** (E, *Helper
  operations*, steps 1–7: host, admission, creation, observation, handoff,
  stop trigger, transfer); this section adds only the inputs and outcomes.
  Spec: stage `ClipboardHelper`; program resolved with C §4.4 against
  cpf's own `PATH` (not found → `unavailable{tool_missing}`, no host
  opened); cpf's environment; cwd the workspace root; stdin `File` = the
  published primary file inside the version (no stdin write can block
  cpf); stdout and stderr `Null`; wall deadline 2 s (*fixed*). Not a
  primary execution, not counted in any plan, nothing persisted.
- Outcomes: exit without a stop trigger and `Exited{0}` → `copied`
  (cleanup `Released`: a forked `wl-copy`/`xclip` selection server keeps
  running and is handed off); another exit or a signal → `failed{exit}`;
  the 2 s deadline → `failed{timeout}` (the group killed and verified;
  a verification other than `Confirmed` is in the message); any
  `NotStarted`, `Unknown` status or `Transferred` → `failed{helper}` with
  the reason. The 2 s wall deadline starts when creation returns (E §3.4;
  `setup_duration` before it is outside the guarantee, as for every run);
  bound from creation return to the outcome: 2 s + the reap deadline +
  the cleanup verification deadline (E §5.5).
- Cancellation: intent set before the host opens → `skipped{cancelled}`;
  intent during the run → the stop-trigger path, `skipped{cancelled}`. The
  version stays published. A `submit` job then follows R §2.9 (intent after
  the publication cutoff); source copy exits with the cancellation status
  of R §5.3. In stage mode the helper runs at phase A after publication.
- **Windows** starts no process: cpf uses the Win32 clipboard API
  through the existing `windows-sys` 0.61 dependency, with features added
  for it (to be verified when implemented: `Win32_System_DataExchange` for
  `OpenClipboard`/`EmptyClipboard`/`SetClipboardData`/`CloseClipboard`,
  `Win32_System_Memory` for `GlobalAlloc`/`GlobalLock`/`GlobalFree`,
  `Win32_System_Ole` for `CF_UNICODETEXT`, `Win32_UI_WindowsAndMessaging`
  for the owner window). The bytes MUST be UTF-8 (else
  `skipped{not_text}`); they are converted to UTF-16 with a terminating
  NUL. cpf creates a message-only window (`CreateWindowExW` with
  `HWND_MESSAGE`) as the clipboard owner, `OpenClipboard(hwnd)` (retried
  up to 10 times, 50 ms apart, while another program holds it),
  `EmptyClipboard`, `GlobalAlloc(GMEM_MOVEABLE)`, copies the text,
  `SetClipboardData(CF_UNICODETEXT)`, `CloseClipboard`, `DestroyWindow`.
  On a failure the memory is freed (it belongs to the system only after a
  successful `SetClipboardData`) and the warning is `failed{win32}` with
  the error code. So source copy on Windows starts no process, with or
  without `--copy`. Unix helpers receive the bytes as they are.

### 9.4 Submit URL

Informative; cpf never requests it.

| Identity | URL |
|---|---|
| `atcoder:<t>` with `[problem] url` = `https://atcoder.jp/contests/<c>/tasks/<t>` (sites §8.4 canonical form) | `https://atcoder.jp/contests/<c>/submit?taskScreenName=<t>` |
| `cses:<n>` | `https://cses.fi/problemset/submit/<n>/` |
| `local:…`, or an AtCoder `url` not in canonical form | none (`null`) |

## 10. Staleness and `cpf status`

For each listed problem, `status` reads `current` and its manifest and
reports *(submission, draft)*:

| State | Condition |
|---|---|
| `none` | no `submit/<p>/` or no `current` |
| `current` | `current` and its manifest `valid`, the manifest's `problem` equals the identity, and the current input list equals the manifest's `[[input]]` paths in order with equal SHA-256 for every file |
| `stale` | as `current`, but the list differs (a file added, removed or reordered) or a file's hash differs, or a file is missing, or (stage mode) the effective solution-file list differs from `[[solution]]` in members or order |
| `unknown` | `current` not `valid`, dangling, identity mismatch, the configuration cannot be loaded, or a file cannot be read (reason in `diagnostics`) |

- The **current input list** is computed for the manifest's `mode`:
  `source_copy` → the effective solution files; `stage` → the captured
  files. Files are hashed with streaming SHA-256 (no copy), regular files
  only, no link component.
- Configuration changes other than the file lists (submit stage argv,
  flags, tool version) do not make a version stale; the manifest records
  them for the user.
- Human output: `submission: <vid> (<mode>, tested|untested), current` or
  `… stale (main.cpp changed)`.
- The status document's `Problem` gains `submit: {state: token, id:
  string?, mode: token?, tested: bool?}` (R §6.1 *(submission, draft)*).

## 11. Maintenance

### 11.1 `cpf clean`

Plan mode lists, `--apply` removes, for each `.cpforge/submit/<p>/`:

| `current` | Candidates |
|---|---|
| `valid`, its version present with a `valid` manifest | every other entry of version-id grammar whose manifest is not `too_new` |
| `missing` | every entry of version-id grammar whose manifest is not `too_new` (versions left by a crash before the first reference, §13) |
| `unreadable`, `malformed`, `too_new`, dangling | none (reported) |

- `--apply`: under the workspace lock, rename each candidate to
  `submit/<p>/.reclaim-<vid>`; release the lock; delete each with the
  no-follow walker (S §6.3 steps 2–3 pattern; a rename never overwrites an
  existing `.reclaim-<vid>`: skipped, reported). A failed rename or
  deletion → exit 9 (retried next time). An existing `.reclaim-*` is
  deleted.
- Never removed: `current`, the referenced version, a version with a
  `too_new` manifest, entries outside the grammar (reported), and the
  `submit/<p>/` directories themselves.
- Job directories with an external record are pruned per §7.4.

### 11.2 `cpf fsck`

Reports: per `submit/<p>/`, the classification of `current` (and
dangling); versions whose manifest is not `valid`, whose `id` differs from
the directory name, or whose `problem` differs from the problem's
identity; files missing or of another size than the manifest says
(contents are not hashed by `fsck`); unexpected entries (neither
`current`, a version id, `.reclaim-<vid>` nor a `.cpf-tmp-` leftover);
`submit/<p>/` without a problem directory (an orphan); job directories
with an external record (§7.4). `--apply` changes nothing under
`.cpforge/submit/` and never removes `D`: there is no cpf record to repair
there, a dangling `current` is fixed by the next `submit-prep`, and
removal is `clean`'s.

### 11.3 Leftover recovery, schema, gitignore, archive

- Each `.cpforge/submit/<p>/` is a **metadata recovery location** (S
  §4.5 *(submission, draft)*): `.cpf-tmp-` leftovers of a `current`
  replace are removed there. Staging leftovers are in `.cpforge/staging/`
  (removed by recovery as every staging entry). `.reclaim-<vid>` entries
  are deleted by recovery too.
- Newer schema: a `too_new` `current` is never replaced (`submit-prep`
  refuses with exit 8, "needs a newer cpf"); a `too_new` manifest is
  never pruned; a `too_new` external record keeps its job directory.
- `cpf gitignore` prints `.cpforge/submit/` as an active line after
  `.cpforge/downloads/`: versions are regenerable tool output.
- Archive (archive contract): the `submit/<p>/` tree is class `submit`,
  omitted unless kept, retired with the problem and never restored.
  Archive refuses while a job lease is held, so a running `submit` job
  blocks it; source copy holds the workspace lock and is serialized with
  it.

## 12. Platforms

| Operation | Linux, macOS | Windows |
|---|---|---|
| Source copy (§5), `current`, output, URL | full | full (no process) |
| Clipboard (`--copy`) | supervisor helper operation, best effort (§9.3) | Win32 clipboard API through `windows-sys`, no process (§9.3) |
| Stage mode: probe, submit stage, artifact test | full | refused, exit 10, at §4.3 step 5: after configuration phases 1–2 and the early refusals (exit 7 first), before the lock, phase 3 (tool resolution), job admission and any mutation; no version, no job directory, no `D` |
| `status` staleness, `clean` pruning, `fsck`, recovery of `D` | full | full (storage only; a job directory with an external record can reach Windows only in a workspace shared with Unix, and is handled the same) |

## 13. Crash states

Kill points use the storage crash harness (S §10, a kill of the real `cpf`
binary at a named point). **Recovery** is one `cpf clean --apply`: a
locked operation, so leftover recovery (S §4.5) runs first; it then prunes
the dead job (and its `D`, §7.4) and the unreferenced versions (§11.1). Every cell also
asserts: `problems/` byte-identical to before; **before the reconciler**,
every version present before the operation byte-identical and present;
**after it**, the set of versions equals the row's *survivors* exactly and
each survivor is byte-identical to its state before the operation (or, for
the new `V1`, to the uninterrupted run's `V1` with its `id` and
`created_at` substituted); no `current` naming an absent version or a
version whose manifest is not `valid`; no `.cpf-tmp-` name and nothing in
`.cpforge/staging/` after recovery.

Scenario **S1**: source copy, problem with `current` naming `V0`.
Scenario **S0**: as S1 with no `submit/<p>/` yet. Scenario **J**: stage
mode with the artifact test, `current` naming `V0`.

| # | Crash point | Scen. | Left by the kill | After recovery and `clean --apply` | Survivors |
|---|---|---|---|---|---|
| 1 | `submit.staging_created` (directory, some files) | S1 | staging partly filled | staging removed; `current` = `V0`; exactly `{V0}` | {V0} |
| 2 | `submit.staged` (manifest written, synced) | S1 | staging complete | as 1 | {V0} |
| 3 | `dir_noclobber.renamed`, `submit.version_published` | S1 | `V1` present, `current` = `V0` | `status` shows `V0`; `clean --apply` removes exactly `V1`; `fsck` reports nothing | {V0} |
| 3a | as 3 | S0 | `V1` present, no `current` | `status` state `none`; `clean --apply` removes `V1`; `submit/<p>/` empty | {} |
| 4 | `submit.current_validated` (temp beside `current`, before the rename) | S1 | `V1`, temp, `current` = `V0` | temp removed by recovery; then as 3 | {V0} |
| 5 | `submit.current_replaced` | S1 | `current` = `V1` | `clean --apply` removes exactly `V0` (the only permitted removal of a pre-existing version); `status` `current` (`V1`) | {V1} |
| 6 | `submit.out_validated` | J | job dir with `submit/out/` | job pruned; submission tree `{V0}`, `current` = `V0` | {V0} |
| 7 | `submit.external_recorded` | J | record, no `D` | job pruned; nothing under `B` | {V0} |
| 8 | `submit.external_created` | J | record, empty unmarked `D` | `D` kept (no proven marker, one `test_dir_kept` finding: the documented leak of §7.4); job pruned | {V0} |
| 9 | `submit.external_marked`, `submit.external_filled` | J | marked `D` (partly) filled | `D` removed; job pruned | {V0} |
| 10 | during the first artifact-test run (`submit.test_started#1`, after the harness reaped the run's processes) | J | marked `D`, job dir | as 9 | {V0} |
| 11 | `submit.external_removed` | J | no `D`, record present | job pruned; nothing under `B` | {V0} |
| 12 | `submit.version_staged` (job's `submit/version/` complete) | J | version in the job dir | as 6 | {V0} |
| 13 | `dir_noclobber.renamed` (phase A) | J | `V1` published, `current` = `V0` | as 3 (the job is also pruned) | {V0} |
| 14 | `submit.current_replaced` (phase A) | J | `current` = `V1` | as 5 (the job is also pruned) | {V1} |

Kills inside `clean --apply` (at `submit_clean.renamed#1`, after the first
`.reclaim-<vid>` rename): the next `clean --apply` (or any locked
operation's recovery) deletes the `.reclaim-*` entry and completes the
plan; `current` and its version untouched.

## 14. Tests

Ids `SB-<area><n>`. Counts and contents are part of each expectation.
*Counters* (feature `storage-test-hooks`): the **spawn counter** counts
process creations by cpf (job executions and clipboard helpers), the
**lock-attempt counter** workspace-lock acquisitions; both start at 0 per
invocation and are read back by the harness.
Source copy, publication, maintenance and clipboard (§§14.1–14.4, 14.7)
run on Linux, macOS and Windows; stage mode (§§14.5–14.6) on Linux and
macOS.

### 14.1 Configuration (C §11 fixtures)

| Id | Case | Expected |
|---|---|---|
| SB-C1 | `[languages.cpp.submit] argv = ["distill","--judge","{judge}","-o","{submit_file}","g++","{solution_sources}"]` | valid; `config show` mode `stage`, provenance per key |
| SB-C2 | `{workspace}` or `{problem_dir}` in the submit stage | phase-2 error naming the placeholder |
| SB-C3 | `{artifact}` in the submit stage; `{submit_dir}` in a build stage | phase-2 errors |
| SB-C4 | `version.argv` containing `{sources}`; `expect = "("` | phase-1 errors (two diagnostics, one run) |
| SB-C5 | `config.submit` in `problem.toml` | phase-1 unknown key |
| SB-C6 | a submit table without `argv`/`shell` | phase-2 "submit stage has no command" |
| SB-C7 | `[languages.cpp.submit.windows]` only, validated for Linux | mode `source_copy` on Linux, `stage` for Windows validation |
| SB-C8 | `{submit_file}` with two solution sources; `{judge}` on a local problem without `judge` | phase-3 errors naming the placeholder; no job directory (count of `runs/` entries unchanged) |
| SB-C9 | `submit.test_dir` = the workspace root, = `problems/`, = a link to the workspace, = a missing path | phase-3 errors (four), exit 7, no job directory |

### 14.2 Source copy

| Id | Case | Expected |
|---|---|---|
| SB-Y1 | grader problem, `solution_files` omitted, `sources = ["main.cpp","grader.cpp"]`, `support_files = ["grader.h"]` | exit 0; version `files/` holds exactly `main.cpp`, `grader.cpp`, `grader.h` with the source hashes; manifest 3 `[[input]]`, 3 `[[file]]`, `tested = false`, no `[tool]`; no process spawned (spawn counter of the test hooks = 0) |
| SB-Y2 | Python grader: `sources = ["grader.py"]`, `support_files = ["solution.py"]`, `solution_files = ["solution.py"]` | exactly `solution.py` copied; primary `solution.py` |
| SB-Y3 | Python, `solution_files` omitted, `support_files = ["helper.py"]` (imported by `main.py`) | both copied, in that order |
| SB-Y4 | `solution_files = []` | exit 7 "nothing to submit"; `.cpforge/submit/` absent afterwards; lock never taken (lock-attempt counter 0) |
| SB-Y5 | 257 solution files; one file growing past 64 MiB during the copy (injection) | exit 7 each; nothing under `submit/`; staging empty |
| SB-Y6 | a solution file replaced by a FIFO / a link after phase 3 (injection at `submit.copy#1`) | exit 8; nothing published |
| SB-Y7 | two runs | two versions, `current` names the second; the first byte-identical |
| SB-Y8 | `--profile x`, `--judge x`, `--events` with `--source-copy` | exit 2 each, nothing created |
| SB-Y9 | `answer`-kind problem | exit 7 "unsupported in this version" |
| SB-Y10 | `current` `too_new`; `current` unreadable (permission) | exit 8 "needs a newer cpf" / naming the error; nothing under `.cpforge/staging/` or `submit/<p>/` created; stage mode with a `too_new` `current`: exit 8 before admission (no job directory), and `current` made `too_new` during the job (injection before phase A): `not_published{current_too_new}`, exit 8, the version kept in `<job>/submit/version/files/` (`submission.path` names it and it exists) |
| SB-Y11 | injected `Exists` at step 5 twice, then success | exit 0, the third `<vid>` published; no staging left; injected 3 times → exit 8, nothing published, the staging removed by the next locked operation |

### 14.3 Publication and crash

| Id | Case | Expected |
|---|---|---|
| SB-P1 | every row of §13 for S0, S1 | the row's state, with the common assertions |
| SB-P2 | injected failure of the `current` replace (not committed) | exit 9; `V1` present, `current` = `V0`; `clean --apply` removes `V1` only |
| SB-P3 | `Durability::Uncertain` after step 6 | exit 0 with one `durability` warning |
| SB-P4 | fsync-order trace (S §10 recorder) for one source copy | every rename after the fsync of its files and before the fsync of its directory |
| SB-P5 | id grammar | `v-20261124T153012345Z-1a2b3c4d` valid; uppercase hex, 7 or 9 hex digits, missing `Z`, a 13th month → unexpected entries |

### 14.4 Maintenance and status

| Id | Case | Expected |
|---|---|---|
| SB-M1 | versions `V0`, `V1`, `V2`, `current` = `V1` | `clean` plan lists exactly `V0`, `V2`; `--apply` removes exactly those; `V1` byte-identical |
| SB-M2 | `current` missing, `V0`, `V1` | both removed |
| SB-M3 | `current` malformed / unreadable / `too_new` / dangling | nothing removed (4 cases), each reported |
| SB-M4 | `V0` manifest `too_new`, `current` = `V1` | `V0` kept, reported |
| SB-M5 | injected rename failure on one of two candidates | exit 9; the other removed; the next `clean --apply` removes the first |
| SB-M6 | `fsck` on: orphan `submit/x/`, a stray file `submit/<p>/junk`, a version missing one file, `id` ≠ directory | four findings, exit 0; `--apply` leaves the tree byte-identical |
| SB-M7 | `gitignore` | output equals the S §5.5 list with `.cpforge/submit/` after `.cpforge/downloads/` |
| SB-M8 | `status` after source copy; after editing `main.cpp`; after adding a file to `solution_files`; after deleting `grader.h` (stage manifest) | `current`; `stale`; `stale`; `stale`; status document `submit.state` matches each |
| SB-M10 | stage-mode version; only `solution_files` changed (from omitted to `["main.cpp"]`, files and hashes unchanged) | `stale`, diagnostic names `solution_files` |
| SB-M9 | `status` with `current` dangling, with a manifest of another problem | `unknown`, a diagnostic each |

### 14.5 Stage mode (Linux and macOS)

The fake tool is a `cpf-fixture` program: `distill --version` prints a
configurable line; otherwise it reads its inputs and writes
`{submit_file}`, inlining quoted includes of files beside its input
(**inline** mode) or copying the input unchanged (**leaky** mode), and can
exit non-zero, sleep or write extra files on request. The problem: C++,
`sources = ["main.cpp"]`, `support_files = ["lib.h"]` (`main.cpp`
includes `"lib.h"`), `solution_files` omitted, three tests, `[check] kind
= "checker"` with a `simple` checker built from `checker.cpp`.

| Id | Case | Expected |
|---|---|---|
| SB-S1 | probe declared, line `distill 1.4.2`, `expect = "^distill 1\\.4\\."`, inline mode | exit 0; executions: probe, submit, artifact build, checker build, 3 × (solution, checker) = 10 started; version with `tested = true`, `tool.version = "distill 1.4.2"`, one `[[file]]` `main.cpp` without an `#include "lib.h"`; `D` removed (`external_dir.state = removed`, no `cpforge-submit-*` left under `B`) |
| SB-S2 | `expect = "^distill 2"` | exit 7, verdict `submit_version_mismatch`; 1 execution started, the other 9 `skipped{submit_not_succeeded}`: `build` and `roles[0].build` results `skipped{submit_not_succeeded}`, each of the 3 tests `not_run{submit_not_succeeded}`, no `internal_error` anywhere; no version; no `D` created (no `cpforge-submit-*` under `B`, no external record) |
| SB-S3 | no probe | exit 0; manifest `tool.version = "unpinned"`; one `tool_unpinned` warning |
| SB-S4 | probe prints nothing; prints 300 bytes; prints invalid UTF-8; exits 1 | exit 7 (three cases, mismatch); exit 3 `submit_failed` (exit 1) |
| SB-S5 | `distill` not on `PATH` (stage or probe) | exit 7 refusal naming the program; no job directory; no version |
| SB-S6 | submit stage exits 2; exceeds its deadline (`deadline_s = 1`, sleeps 5 s) | exit 3 `submit_failed` each; no version; `submit/out/` kept in the job |
| SB-S7 | artifact selection rule rows (§6.4): a link in `out/`; a file `a\x01`; 0 files; 257 files; `{submit_file}` not written; an extra file `grader.cpp` colliding with a provided file (grader variant); output renamed to `sol.cpp` | `submit_failed`, `preparation.artifact_invalid` with `reason` `not_regular`, `name`, `empty`, `too_many`, `submit_file_missing`, `provided_collision`, `source_missing` (7 cases) and `path` the offending file where one exists; exit 3, no version, no `D`; with the artifact test planned, the artifact build, checker build and test executions `skipped{submit_not_succeeded}` and each test `not_run{submit_not_succeeded}`; `submission.path` = `<job>/submit/out/`, which exists |
| SB-S8 | `--no-test` | executions probe + submit only; version `tested = false`, no `[test]`; no `D` |
| SB-S9 | the artifact fails test 2 (inline mode, `main.cpp` wrong on test 2) | exit 1; `wrong_answer`; no version; `current` unchanged; `D` removed |
| SB-S10 | zero enabled tests | exit 4 `no_tests`; no version |
| SB-S11 | cancellation (SIGINT) during the artifact test; and between the verdict and the publication cutoff (injection) | verdict `cancelled`, no version (first); `publication.not_attempted{cancelled}` (second); `D` removed in both |
| SB-S12 | inline mode but `main.cpp` includes `lib.h` by its **absolute** workspace path | exit 0, `tested = true`: the documented non-guarantee (§1.2) |
| SB-S13 | build stage with `-I{problem_dir}`; with `-I<abs path of the job's inputs/src>` via an absolute literal; run stage `cwd = "problem"` | phase-3 errors (accident guard rule 1; the §6.6 `inputs/` rule; the §6.6 cwd rule), exit 7, no job directory |
| SB-S14 | while interactive judging and `testlib` are refused by `cpf test`: an interactive problem; a `testlib` checker | exit 7 "unsupported in this version" before admission; with `--no-test` both succeed |
| SB-S15 | publication: injected `current` replace failure; lock `Busy` at phase A | exit 9 `published{current:false}`, `submission.path` the published version (exists); exit 8 `not_published{lock_busy}`, `submission.path` = `<job>/submit/version/files/` and that directory exists with the artifact |
| SB-S16 | `--no-test`: (a) the probe program removed after phase 3 (`NotStarted{Setup}`); (b) SIGINT while the submit stage sleeps; (c) intent set before the submit stage's admission | (a) verdict `infrastructure`, exit 10, the submit stage `skipped{submit_not_succeeded}`; (b) verdict `cancelled`, §5.3 status, `preparation.not_reached`; (c) the submit stage `skipped{cancelled}` (cancellation before `submit_not_succeeded`); no version in all three |
| SB-S17 | manifest size. Paths: 256 relative paths of exactly 2 420 bytes, each 9 directory components `d<k>` padded with `x` to 240 bytes (k = 0…8) then a file name `f<iii>` padded with `y` to 251 bytes (iii = 000…255); every file 1 byte. (a) Renderer vector (pure, every OS): a source-copy manifest with these as `[[input]]` and `[[file]]` renders to 1 294 755 bytes (> 1 048 576) with the reference header; a stage manifest adding `[[solution]]` to more. (b) Linux CLI, workspace root ≤ 200 bytes: source copy with these 256 solution files; stage mode whose fake tool copies them into `out/` | (a) the renderer reports `too_large` for both; (b) source copy exit 7 "submission manifest too large", stage `submit_failed` with `preparation.artifact_invalid{manifest_too_large}`; nothing published in both, staging empty |

### 14.6 External test directory: the isolation seam

Same problem and fixture as §14.5; `submit.test_dir` points at a fresh
directory `B` outside the workspace. Both artifacts go through the same
seam: the same job plan, the same `D` lifecycle, the same checker.

| Id | Case | Expected |
|---|---|---|
| SB-X1 | **positive**: inline mode | the artifact build in `D` succeeds; the checker build succeeds; 3 tests `passed` by the checker (3 checker runs `accepted`); exit 0; during the test (barrier `submit.test_started#1`) `D` holds exactly `.cpforge-submit` and `main.cpp`, and the job directory holds `inputs/src/main.cpp`, `inputs/src/lib.h`, the checker build output and every spec and record so far |
| SB-X2 | **negative**: leaky mode (the artifact still includes `"lib.h"`, which is in `support_files` and so a solution-side file) | the artifact build fails: verdict `build_failed`, exit 3; the build's stderr capture names `lib.h` (`No such file or directory` or the platform compiler's equivalent, matched on the file name); the checker build is `skipped{build_not_succeeded}` (R §2.3 order) and the 3 test runs too; `inputs/src/lib.h` and the submit stage's spec and record are present in the job (cpf's job infrastructure was intact); checker usability is shown by SB-X1 on the same seam and by the control; no version; `D` removed. Control: `cpf test` on the same problem passes (3/3), so the failure comes from the seam, not from the source |
| SB-X3 | grader variant: `sources = ["solution.cpp","grader.cpp"]`, `support_files = ["grader.h"]`, `solution_files = ["solution.cpp"]`, stage uses `{solution_sources}` | `D` holds exactly `.cpforge-submit`, `solution.cpp` (artifact), `grader.cpp`, `grader.h` (provided, hashes equal to the captures); 3 tests pass |
| SB-X4 | Python variant (build-free): `main.py` imports `helper.py` (solution-side); inline mode inlines it, leaky mode does not | inline: 3 tests pass; leaky: every test run fails with `ModuleNotFoundError` naming `helper` (class `nonzero_exit`, exit 1) |
| SB-X5 | §13 rows 6–14 | the row's state; after `clean --apply` no `cpforge-submit-*` under `B` except where the row keeps one |
| SB-X6 | independent process (not cpf) holds a crashed job's lease (row 9 state) while `cpf clean --apply` runs | job directory and `D` byte-identical; after the helper releases the lease, the next `clean --apply` removes both |
| SB-X7 | recovery classification: `D` replaced by a link; `B` replaced by a link; marker replaced by a link to a file with the right token; marker with another token; unmarked empty `D`; unmarked non-empty `D`; record `malformed`; record `unreadable` (permission) | the six `D` cases kept, 6 `test_dir_kept` findings; malformed: job pruned, `test_dir_lost`, `D` kept; unreadable: job directory kept, reported |
| SB-X8 | injected walker failure in §7.3 step 6 | exit status of the verdict unchanged (0); one `submit_test_dir{leftover}` warning; record present; the next `clean --apply` removes `D` |
| SB-X9 | `D` pre-created (empty, foreign) by the test with the chosen name (injection of `<r>`), then `cpf clean --apply` twice | the job: infrastructure, exit 10; first clean: one `test_dir_kept` finding for `D`, the job directory (and its external record) pruned; second clean: no finding for `D` (cpf no longer has a pointer to it); the foreign directory exists and is empty after the job and after both cleans |

### 14.7 Windows and clipboard

| Id | Case | Expected |
|---|---|---|
| SB-W1 | Windows: stage declared, valid configuration | exit 10 before admission; the `.cpforge/` tree (paths and hashes) identical before and after; no version |
| SB-W2 | Windows: stage declared, phase-2 configuration error | exit 7 (configuration first) |
| SB-W3 | Windows: `--source-copy`, and a language without a submit table | exit 0, a version as SB-Y1; in a Job Object, `TotalProcesses` = 1 (cpf alone) |
| SB-W4 | Windows: `--copy`, ASCII and non-ASCII primary file | clipboard read back by the test (`GetClipboardData(CF_UNICODETEXT)`) equals the file decoded as UTF-8; `TotalProcesses` = 1 (no process spawned); exit 0 |
| SB-W5 | Windows: clipboard held open by another thread of the test for 2 s; a non-UTF-8 file | `failed{win32}`, `skipped{not_text}`; exit 0 and the version published in both; `TotalProcesses` = 1 |
| SB-K1 | Linux: `PATH` with a fake `wl-copy` (records stdin, exits 0), `WAYLAND_DISPLAY` set | `copied`; the fake received exactly the file's bytes |
| SB-K2 | Linux: no `WAYLAND_DISPLAY`, no `DISPLAY` | `unavailable{no_display}`, exit 0 |
| SB-K3 | macOS: fake `pbcopy` exits 1; sleeps 10 s | `failed{exit}`, exit 0; `failed{timeout}` within 2 s + 2 s + 1 s, exit 0; reaping is observed inside cpf through the test hook `helper.reaped` (fired after the consuming `wait4` returned the root's pid), and the reported status is not `Unknown` |
| SB-K9 | Linux and macOS: fake helper forks a descendant that sleeps 30 s, then the root (a) sleeps past the 2 s deadline, (b) is cancelled by SIGINT to cpf | (a) `failed{timeout}`, cleanup `Confirmed`, the descendant has *no live identity* (E §10.2: `kill(pid, 0) = ESRCH`, a different start identity, or state `Z`); *entry disappeared* is asserted only on Linux with the harness as child subreaper reaping it; (b) `skipped{cancelled}`, the same cleanup and descendant assertions, source copy exits with the cancellation status, the version published; `Released` in neither |
| SB-K10 | thread credits exhausted (test hook holds all 32) | `failed{helper}` (`NotStarted{Worker}`), no process created (spawn counter 0), exit 0 |
| SB-K6 | blocked reader: fake `wl-copy` never reads stdin and sleeps 10 s | `failed{timeout}` within the bound of §9.3, cpf never blocked on a write (stdin is the version file) |
| SB-K7 | successful handoff: fake `xclip` forks a child that keeps stdin, stdout and stderr open and sleeps 30 s, the root exits 0 | `copied` with cleanup `Released`, `helper.reaped` fired; cpf exits within 1 s of the root's exit; the child is still alive afterwards (handed off) and is killed by the harness |
| SB-K8 | ownership audit (Linux, macOS) | the clipboard path creates the child only through the supervisor's creation transaction (no `Command::spawn` outside `supervisor::sys`, the existing CI check) |
| SB-K4 | two solution files, `--copy` | `skipped{no_primary}`; 5 MiB primary → `skipped{too_large}` |
| SB-K5 | `--copy` without a committed version (exit 8 path) | no helper started (spawn counter 0) |

### 14.8 Wire (after the final schema reconciliation)

| Id | Case | Expected |
|---|---|---|
| SB-R1 | `submit.json` (source copy), `submit_stage.json` (stage `job_report`) fixtures | encoder vectors and the Python oracle round trip |
| SB-R2 | worst-case `Submission` (16 files with 128-byte names of escaped characters, `nsum` maxima) | serialized ≤ 17 408 B, and the computed 16 942 B is reached by the vector |
| SB-R3 | exit precedence: `submit_version_mismatch` → 7; `submit_failed` → 3; `published{current:false}` + `passed` → 9; `not_published` + `passed` → 8; machine-output delivery failure with any of those → 11; internal failure + delivery failure → 70; forced cancellation + delivery failure → R §5.3 status; orderly cancellation + delivery failure → 11 (R §5.2 pairs) | each status |

## 15. Amendments to other documents

Applied with this contract, marked *(submission, draft)*:

- **C §1.1** rows `languages.<l>.submit`, `submit.clipboard`,
  `submit.test_dir`; the language table gains `submit`; the sentence
  "Keys from later designs (`submit`, …)" drops `submit`. **C §2** overlay
  sites gain `languages.<l>.submit`. **C §5.2** the four placeholder rows
  (§3.4). **C §9** table row `submit-prep`. **C §11** fixture bullet
  (§14.1).
- **E §2** stages `Submit`, `SubmitProbe`; **E §5.5** defaults: submit
  stage as build (1 MiB streams, 60 s), probe 64 KiB / 10 s (*fixed*).
- **S** header; **§3** ownership rows (`.cpforge/submit/<p>/current`,
  `<vid>/`, the job's `submit/`), reserved trees; **§4.5** metadata
  recovery location and `.reclaim-<vid>`; **§5.5** gitignore line; **§5.6**
  fsck; **§6.1** job layout; **§6.3** pruning (external record, step 3)
  and `clean` (submission versions); **§7** accident guard for the
  artifact test; **§9** governed formats; **§10** commit-point rows.
- **R** header; **§1** job kind `submit`; **§2.7** verdicts; **§2.9**
  phase A; **§4.1** `command` token `submit_prep` and member
  `submission`; **§4.5** stage tokens; **§4.10** warnings; **§5.1**,
  **§5.2**, **§5.5** exit rows; **§6.1** `submit` document and the status
  member.
- **commands.md** "Cleaning": submission versions and test directories.
- **workspace design §2.1** row `.cpforge/submit/`; **§3.3** `clean` scope
  (it limited `clean` to `runs/` and `cache/`).
- Design §6.7 ("pinned by version", "builds and tests the artifact … when
  the language allows", "headers from the workspace cannot leak in") is
  ratified by the design-ratification item with the narrowings of §1.2.

## 16. Decisions and open points

Settled here (engineering):

- Source copy is a locked storage command, not a job, so it runs on
  Windows; stage mode is a job.
- The external test directory receives the provided files, not every
  declared support file: a solution-side support file in `D` would let a
  workspace-dependent artifact pass. Role binaries stay in the job (judges
  run in their private directories there); copying them into `D` would not
  change what the artifact can see.
- Publication only for a fully judged artifact or an explicitly untested
  one; `current` is never moved to a failing artifact.
- `{solution_sources}` added so that grader tasks distil the contestant's
  file only.
- `.cpforge/submit/` is an active gitignore line.

Settled by the user:

- `{judge}` is the site of a fetched problem (`atcoder`, `cses`); the
  submit table's `judge` or `--judge` sets it for anything else.
- The clipboard is off by default, opt-in with `--copy` or
  `submit.clipboard = true`: `pbcopy` on macOS, `wl-copy`/`xclip` on Linux,
  the Win32 clipboard API on Windows (no process, §9.3).
- The external test directory defaults to the OS temporary directory.
- `.cpforge/submit/` is gitignored.
- The artifact test uses the same `--profile` as `cpf test`.
