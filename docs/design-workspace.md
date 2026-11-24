# Managed workspaces (design)

Status: **v9, planned**. Managed workspaces combine tracking within a
workspace, one main workspace and several registered workspaces. Extends
`design.md` §4; storage rules are `contracts/storage.md` (*S §n*),
configuration `contracts/config.md` (*C §n*), execution
`contracts/execution.md` (*E §n*). Where this design needs a
contract change it says so (§9); nothing here silently amends a contract.

**Boundary:** cpf does **not** do what git does: no
history, versions, diffs of user files, backups or exports of content. The
user keeps the workspace in a git repository of their own if they want one;
cpf at most **offers** git conveniences (§7). cpf's tracking covers only what
cpf alone knows: what it downloaded (hashes), what it owns, what it ran and
with which result. The archive (design §7a) keeps its retire-with-snapshot
semantics as the one explicit exception.

**Scope split:** §2–§4 and the single-workspace rows of §9 are
**normative for the planned single-workspace features**. §5–§6 are
**later-version sketches**, not to be implemented until their listed
blockers are resolved; the one exception is the fetch provenance of §6.4,
which ships with fetch and is normative through §2.2. The corresponding contract amendments remain
**draft, not yet implemented**.

## 1. Design decisions

| Question | Decision |
|---|---|
| Views made of links (§6.3) | yes |
| `.gitignore` for tool-owned content | cpf offers it, never writes it unasked |
| Default path of the main workspace (§6.1) | none: the user defines it at least once |

## 2. Ownership and file attributes (single-workspace features)

### 2.1 Reserved trees

Exactly these trees are **tool-owned in their entirety**: everything inside
them belongs to cpf, whoever created it, and the documentation says so:

| Tree | Disposable | Notes |
|---|---|---|
| `<root>/.cpforge/runs/` | yes: S §6.3 pruning, plus the owning job's own cleanup (S §6.4: overflowing `cwd/`, oversized builds) | job directories incl. builds and `cwd/` (S §6.1, §7) |
| `<root>/.cpforge/cache/` | yes | HTTP page cache of `cpf fetch` (sites contract §6); the per-host request permit lives in the user cache directory, outside the workspace |
| `<root>/.cpforge/staging/` | only by its owner protocol (S §3) | |
| `<root>/.cpforge/facts/` | no | run facts (§3.2) |
| `<root>/.cpforge/state.toml`, `lock` | no | S §3 |
| `<root>/.cpforge/job-seq` | no | admission sequence (§3.2) |
| `<root>/.cpforge/downloads/` | only by its lease protocol (storage contract §5.8.3) | download areas of running fetches |
| `<root>/.cpforge/fetch/` | no | fetch journals, history and status (storage contract §5.8.2); moved with its problem by archive retirement |
| `<root>/.cpforge/archive/` | no: each transaction directory is removed by its own protocol | archive and restore transactions (archive contract §5.3, §7.3) |
| `<root>/.cpforge/quarantine/` | no: only `cpf quarantine purge --apply` | files displaced by archive cleanup; user content in a tool tree (archive contract §6.6) |
| `<root>/.cpforge/submit/` | only unreferenced versions, by `cpf clean` | submission versions and their `current` reference (submission contract §8) |

There is **no** reserved or disposable path under `problems/` or
`contests/`: builds and runs live in job directories (S §6.1). Every path
outside the reserved trees is **protected**: no command deletes it
automatically, whoever created it. The one approved
transaction that relocates protected paths is `cpf archive --apply`
(archive contract): after a committed snapshot it moves the problem
directory into `.cpforge/archive/<txn>/` and its cleanup sends files to the
OS trash, the quarantine or, under `delete`, removes them. Saved stress
failures under `problems/<p>/stress/failures/` are written by cpf but are
user content once published (stress contract §7), so they are protected
like any other file there. File-based I/O runs only in the run's own
`cwd/` inside its job directory (execution contract §5.6); a file-based I/O
problem with a shared `cwd` is a configuration error (C §6.2), so no
program output lands in a protected path through it.

### 2.2 Attributes of protected paths

For paths under a problem directory, two independent attributes:

| Attribute | Values | Source (precedence top to bottom) |
|---|---|---|
| **provenance** | `registered` (a `tests.toml` entry or `problem.toml` field names it) \| `created{command}` \| `adopted` \| `unknown` | `tests.toml` / `problem.toml` references first; then `[files] created` (recorded by the cpf command that created a file **no other record names**, i.e. the template solution; a test added by `cpf test add` is already `registered`, storage contract §5.4); then `[files] user` (`cpf adopt`); otherwise `unknown` |
| **visibility** | `normal` \| `ignored` | `[workspace] ignore` patterns (§2.4) |

A path in two lists takes the first matching row; `fsck` reports such
overlaps. The fixed layout names `problem.toml`, `tests.toml` and
`notes.md` are part of the problem itself, never
`unknown` or untracked (storage contract §5.9.1). Downloaded provenance `downloaded{role, txn}` (fetch, storage
contract §5.8.10) sits between `registered` and `created{command}`: a file
under `tests/` that no record names is `downloaded` only when its name and
current SHA-256 appear in the open fetch journal or a valid fetch history
record of the problem (several matches: the role precedence and txn rule
of storage contract §5.8.10, independent of record order and clocks);
otherwise it is `unknown`. **Untracked** = provenance `unknown` and
visibility `normal`. Neither downloaded nor untracked files are ever removed
by automatic cleaning (`clean`, pruning, `fsck`). Only an approved,
inventory-bound transaction (archive; later `move`, §6.2) may relocate
them, and its plan lists them. In an archive each file falls into an
archive class (archive contract §3.3: superseded downloads are
`tests:sample`, revision pairs `tests:custom`, untracked files with a
language extension `sources`), and it is left out of the snapshot only
when an explicit `discard` rule names that class (archive contract §3.4);
files of class `other` are always kept.

### 2.3 Adoption

`cpf adopt <path>`: adds the path to `problem.toml` `[files] user = [...]`
by the shared-file edit transaction (S §4.4). Paths are relative to the
problem directory, validated (no `..`, no link component, S §2; must exist
and be a regular file). It changes provenance only.

### 2.4 Ignore patterns

`cpforge.toml` is never rewritten (S §3); `cpf ignore <pattern>` **prints**
the line to add (`[workspace] ignore = ["*.swp", ...]`, gitignore-style glob
subset defined in the config amendment, §9). Ignored files are hidden from
the untracked lists of `cpf status`; nothing else changes.

## 3. Commands on one workspace (single-workspace features)

### 3.1 Single-workspace selection

Single-workspace commands have no registry. Every workspace command selects by: `--workspace
<path>` → discovered workspace (S §2) → error. An unavailable or invalid
selected root is an error. **Exempt** from selection: `cpf init`, `cpf
--help`, `cpf --version`, `cpf doctor`. Named selection (`-w`) and the main
workspace arrive with the registry (§6.1).

### 3.2 Run facts

- One fact per **(problem, solution target)**, `.cpforge/facts/<problem>/
  <target>.toml`, schema-versioned, atomic replace under the workspace lock.
- **Admission sequence**: a pair `(epoch, n)`. `epoch` is a random
  128-bit id, `n` a `u64`. The counter lives in its own tool-owned file
  `.cpforge/job-seq` (`{ schema, epoch, next }`), not in `state.toml`.
  Allocation, under the workspace lock in the admission critical section
  (S §6.2): read `next`, atomically replace the file with `next + 1` (S §4.2)
  **before** the job directory exists, then write `(epoch, n)` into the new
  job's `job.toml`. A crash between the two leaves an unused number (a gap),
  never a reuse. `next = u64::MAX` refuses admission (unreachable in
  practice, reported). **Initial creation**: `cpf init` creates
  `job-seq` with a fresh epoch (no-clobber) **only when `cpforge.toml` was
  absent when that `init` started**, and before it publishes `cpforge.toml`
  (which S §5.1 writes last). A rerun of `init` on an existing workspace
  never creates `job-seq`. An `init` interrupted before `cpforge.toml`
  exists is simply rerun as an initial creation; a `job-seq` it already
  wrote is kept (no-clobber). In an existing workspace admission **fails
  closed** unless `job-seq` is `valid` (S §9 classification; S §6.2 step
  2a): `missing` or `malformed` → run `cpf fsck --apply`, which starts a
  **new epoch** (`next = 0`) **only when no job lease is held or
  unopenable** (probed as in S §6.3; otherwise it refuses and lists those
  jobs); `unreadable` → the I/O error is reported and the file is
  preserved until access is fixed (same epoch) or the user removes it
  (then `missing`); `too_new` → preserved, a newer cpf is needed
  (S §5.6). A sequence is never rebuilt from surviving records,
  so `(epoch, n)` is never reused.
- **Source capture:** at admission the job copies the solution target's
  captured files (`sources` then `support_files`, C §6.2) into its job directory (`inputs/src/`, like frozen tests,
  S §7), and every expansion of `{sources}` / `{source}` in that job, in any
  stage, names the copy. The `source` fingerprint therefore names the bytes
  **cpf handed to the job**. cpf does not sandbox builds: a build that reads
  original workspace files by other means (a path inside a shell script, an
  absolute `#include` inside a source, `CPATH` or similar environment
  variables, a wrapper program) is not detected. The plan rejects argument
  operands that point into the workspace outside the job (S §7) as a guard
  against accidents, not as enforcement. Each job records a **capture
  check**: `checked` when its source-consuming stage is an `argv` stage
  whose operands passed that guard, otherwise `unchecked{reasons}` (a shell
  stage, whose script text is not inspected). The job report, the fact and
  `status` carry it, so an `unchanged` from an unchecked capture is shown
  as such. Each declared judge role (checker, validator, interactor) is
  captured the same way into `inputs/roles/<role>/` with its own capture
  check (C §9.1.3).
- **Fingerprints**: SHA-256 over a byte string built with
  conventions like E §2's canonical encoding, fully restated here: every integer `u64` little-endian; every
  byte string length-prefixed (`u64` length, then bytes); paths relative to
  the problem directory, components joined by `/`, as raw bytes on Unix and
  WTF-8 on Windows; order by ascending byte order of the encoded path or id.
  - `source`: `"cpf-fp-source-v1"` (as a byte string), file count, then
    per file of `inputs/src/` its path and content (each a byte string);
    then the build profile name (a byte string, empty when no profile is
    selected); then
    the **resolved build command**, encoded here and not by reference:
    these **covered build settings** after config resolution (C §2, §4): whether a build stage exists, its command (`argv` or
    `shell`), `cwd`, `env_remove` and `env`. Not covered: `deadline_s`,
    `stdout_cap`, `stderr_cap`, `limits` and the inherited environment.
    `unchanged` therefore means **equality of these recorded inputs**, not
    equivalent build output or execution behaviour. Encoded:
    first the target's **source list in configured order** (C §6.2: count,
    each problem-relative path as a byte string), since it fixes the order
    in which `{sources}` expands; then its `support_files` list the same
    way (count 0 when none), so moving a file between the two roles changes
    the fingerprint although no byte changes; then the **effective entry**
    (C §6.4) as a tag byte `0` = none or `1` followed by the entry as a
    byte string, since a build may expand `{entry}` and Java's required
    output derives from it (`solution_files` is not covered: it changes no
    build input); then
    one tag byte `0` = build-free target (nothing follows) or `1` = build
    stage, followed by the stage **as merged and validated but not
    expanded**: placeholders stay as their literal text (`{sources}`,
    `{artifact}`), so job-specific paths never enter the fingerprint and
    `status` can recompute it from configuration alone; program resolution
    (C §4.4) is not included (like the toolchain). The command is a tag byte
    `0` = `argv` (count, each argument as a byte string) or `1` = `shell`
    (C §5.3), followed by the interpreter (count, each element as a byte
    string, as configured), the script (byte string) and the args (count,
    each as a byte string);
    the `cwd` as a tag byte `0` = selector, followed by one byte `0` =
    `run` (also the default when unset), `1` = `problem`, `2` =
    `workspace`; or `1` = path, followed by the config-relative path as
    joined lexically at load (C §4.2: never canonicalised; `~/` already
    expanded), as a byte string; the effective `env_remove`
    names (count, each a byte string, ascending byte order); the effective
    `env` map (count, ascending byte order of the name, each the name and a
    tag byte `1` = set followed by the value, or `0` = unset, `NAME =
    false`). Strings are raw bytes on Unix, WTF-8 on Windows. The inherited
    baseline environment (C §4.3 step 1) is not part of the fingerprint.
  - `tests`: `"cpf-fp-tests-v1"`, test count, then per enabled frozen test
    (S §7) its id, input bytes, and the answer as one tag byte (`0` absent,
    `1` present) followed, when present, by its bytes (so empty and absent
    differ).
  - `judge`: `"cpf-fp-judge-v1"` over the effective `[check]` settings and
    every declared role's captured files, lists, build and run stages and
    outcome-affecting limits, in the same conventions; its exact encoding
    is C §9.1.3. It is absent for a problem without a role. Without it a
    checker or interactor edit would leave `status` reporting a result as
    `unchanged` that the current judges never produced.
- **Publication** at job finalization: the fact file's atomic replace is
  the **commit point** (S §4.1), done before the job's lease is released, so
  S §6.3 cannot prune a finished job before its fact exists.
- **Failure rule**: a job interrupted **before** the commit (death,
  cancellation, publication error, reported by that job as a warning that
  does not change its exit code, storage contract §6.5) leaves the previous fact in place, and `status` calls it the **last
  completed run**; its directory becomes prunable as usual. A job
  interrupted **after** the commit keeps its new fact. Post-commit
  `Durability::Uncertain` or `Cleanup::Pending` (S §4.1) are warnings, not
  publication failures. Nothing is recovered from interrupted jobs. Only
  committed facts compete in ordering.
- **Epochs at publication**: under the workspace lock, a job whose
  epoch differs from the current `job-seq` epoch publishes **no** fact (it
  reports this; nothing else changes). Since a new epoch starts only with no
  live leases, this happens only for a job admitted before a recovery that
  raced it, which the lease probe excludes; the rule is the fail-closed
  backstop. An unreadable `job-seq` at publication also publishes no fact.
- **Ordering:** a fact of the current epoch replaces any fact of another
  epoch (facts carry their epoch); within one epoch a higher `n` wins; never
  a lower one. Epochs carry no chronology beyond "current or not".
- `status` compares current files with the fact by recomputing the
  fingerprints: `source changed`, `tests changed`, `judge changed`,
  `unchanged`, or `unknown` (no fact; target not fingerprintable; schema
  mismatch). The claim is
  scoped to recorded inputs, and `status` shows the fact's capture check
  next to it (`unchanged (capture unchecked: shell stage)`).
- **Golden vectors** (per OS) fix the encoding, at least: the same files
  and settings admitted in two different job directories give the same
  `source` fingerprint, equal to what `status` recomputes; reordering the
  configured source list changes it; changing a shell stage's interpreter,
  script or one of its args changes it; an edit to an original file after
  capture does not change the job's fingerprint but makes `status` report
  `source changed`.

The current configuration model has one unnamed solution target. Its fact
key is `solution`, independent of the selected language or profile; those
settings describe inputs to that same target. The problem directory key is
the validated local problem name (without `local:`).

Schema-1 fact fields are `schema`, `epoch`, `n`, `job`, `finished_at`,
`verdict`, `source`, `tests`, `judge` (optional: absent means no judge
role), and `[capture_check]`. The capture check uses
`kind = "checked"` or `kind = "unchecked"` with `reasons = ["shell_stage"]`,
matching report vocabulary. `finished_at` uses the report's UTC millisecond
timestamp format and records completion of the primary work at phase A;
after-test hooks and report settlement may finish later. Verdict summaries
use the report's primary test-job verdict token. Extra fields in supported
schemas are ignored, as with the other tool-only record codecs.

### 3.3 Commands

- **`cpf status [problem|contest|--all]`**: per problem and target:
  registered files missing, untracked files, run-fact comparison (§3.2) with
  last verdict and time; explicit user status (tags and the workflow
  status set by `cpf mark`, design §7a; storage contract §5.9) kept apart
  from derived verdict facts; whether the judges changed since the fact
  (§3.2); the current submission version and whether it is stale
  (submission contract §10); the latest fetch attempt and any incomplete
  fetch (storage contract §5.8.9); `--json` (design §8); the shared filters
  of storage contract §5.9.6. Never compares against history.
- **`cpf clean [--apply]`**: plan by default. It is **S §6.3
  pruning with a zero retention budget**, plus deleting `.cpforge/cache/`
  (initial feature release) — the same leases, lock and two-step rename-then-delete protocol,
  so active jobs are excluded through deletion by the existing mechanism. It
  never touches anything outside `.cpforge/runs/` and `.cpforge/cache/`,
  except the unreferenced submission versions under
  `.cpforge/submit/` and the external test directories recorded by pruned
  `submit` jobs (submission contract §§7.4, 11.1); never `facts/`,
  `job-seq`, `lock`, `state.toml`, `fetch/`, `archive/`, `quarantine/` or
  a referenced submission version. Download areas left by an interrupted
  fetch are removed by the leftover recovery that every locking command
  runs, `clean` included, and only when their lease is free (storage
  contract §5.8.3). Saved stress failures are user content and never
  cleaned.
- **`cpf adopt <path>`**, **`cpf ignore <pattern>`**: §2.3, §2.4.
- **`cpf fsck [--apply]`**: takes the workspace lock; reports dangling
  contest references, problem directories without `problem.toml`, schema
  versions, provenance overlaps (§2.2), facts for unknown problems/targets,
  unexpected entries in reserved trees. `--apply` repairs **only cpf
  records** (removes dangling references by the edit transaction, removes
  facts of deleted problems); never user files, never `.cpforge/lock`, never
  infers lock abandonment from age or PIDs.

## 4. Git conveniences (single-workspace features, offered only)

- `cpf gitignore [--write]`: prints patterns for tool-owned content
  (`.cpforge/runs/`, `cache/`, `staging/`, `downloads/`, `submit/`,
  `archive/`, `quarantine/`, later `views/`); `.cpforge/facts/`,
  `.cpforge/fetch/` and `problems/*/stress/failures/` are offered only as
  comments (S §5.5): fetch history is what makes downloaded tests
  recognisable as downloads, and saved failures are user work a user may
  want to commit. `--write` maintains one
  delimited block in `<root>/.gitignore` (created no-clobber if absent;
  otherwise only its own block is replaced, by atomic replace of the file
  after verifying the rest is unchanged since read). `cpf init` mentions the
  command when it detects a git repository; it never writes the file itself.
- Nothing else: no commit, history or git hooks.

## 5. Dropped (git covers them)

Export/backup tarballs, `.local` copies or any snapshot of user files beyond
the archive exception, diff viewers for user files, in-place restore.

## 6. Later workspace features (not normative yet, except §6.4 provenance)

### 6.1 Registry and main workspace

`$XDG_CONFIG_HOME/cpforge/workspaces.toml` (C §1 directory rules),
schema-versioned, under a persistent registry lock with atomic replace;
entries `{ id, name, path }` and the single authoritative `main` id (no
separate `role` field). Identity: UUID in tool-owned `.cpforge/
identity.toml`; duplicate ids on two live paths are an identity conflict
rejected before any id-based lock ordering; `relocate` / `reidentify` as
repair commands. No default main path: `cpf init --main <path>` or `cpf ws
add --main <path>`. Selection adds `-w <name>` and, for creation commands
outside any workspace only, the main workspace; global commands (`cpf ws
…`) are exempt from selection. Cross-workspace `ls` / `search`.
**Blockers:** recoverable `reidentify` across identity file and
registry; validation of `relocate` against live copies; lock order with
duplicate detection first.

### 6.2 `move`

Plan + `--apply`, both workspace locks in id order, job admission blocked
for the problem in both workspaces, journal before any mutation, no-replace
publication, idempotent reference updates, retire source last, keep both on
doubt. **Blockers:** editor saves during the move (locks do not
exclude them: re-verify the inventory after staging and abort on change),
nested/overlapping workspaces rejected by physical containment, durable
intent and recovery for every crash point, the retirement step's relation
to the no-backup boundary.

### 6.3 Views

Generated link trees under an owned `views/` (owner marker file), symlinks
or junctions, sanitised labels, targets validated as problems of this
workspace; never traversed through. **Blockers:** walkers that must
inspect or remove link entries (clean, fsck) vs. walkers that skip the tree;
discovery order for a cwd inside a view (logical path, canonical target,
workspace); validation of existing entry types.

### 6.4 Fetch provenance and retained originals

Fetch provenance and edited-download detection ship with fetch and are
normative now: provenance `downloaded{role, txn}` in §2.2, decided by the
fetch journal and history (storage contract §5.8.10); a sample pair is
edited when either file's bytes differ from the hash its catalog entry
recorded, or a file is missing (storage contract §5.8.5).

Retained originals are **deferred**: a content-addressed
`.cpforge/originals/` store with `cpf tests original <test> --to <path>`
(no-clobber extraction to a validated new path outside reserved trees,
provenance `created{tests original}`). Their purpose, keeping the upstream
version of a sample the user edited, is met without a second store: fetch
publishes the upstream pair no-clobber as an unregistered revision pair in
`tests/` and records it in the durable fetch history, which moves with the
problem into an archive (design §4.3). A store would add its own
publication order, recovery, corruption handling and garbage collection.
**Blockers** if it is wanted later: publish and verify an original before
the referencing `tests.toml` commit; missing/corrupt objects;
reference-safe collection.

`[downloads]` for non-test assets is deferred with it: this version fetches
no attachments (sites contract §1).

## 7. Import (deferred)

`cpf import <dir>` needs its own spec (identifying problems from paths,
preservation and ownership rules) before any work.

## 8. Open points for single-workspace implementation

None beyond the contract amendments in §9.

## 9. Contract changes

| Contract | Change | Feature scope |
|---|---|---|
| S §3 ownership | add `.cpforge/facts/` (tool, atomic replace); state that reserved trees (§2.1) are tool-owned in their entirety and that no path under `problems/`/`contests/` is disposable | single-workspace features |
| S §3 / problem.toml schema | user field `[files] user = [paths]`, tool field `[files] created = [{ path, command }]`; validation rules of §2.3 | single-workspace features |
| S §6.2 admission | `(epoch, n)` sequence in tool-owned `.cpforge/job-seq`, allocated (counter replaced) before the job directory exists; `job.toml` records it; refusal and new-epoch recovery via `fsck` | single-workspace features |
| S §3 / S §9 schemas | `job-seq` and `job.toml` (schema-versioned, tool-owned); ownership rows for both | single-workspace features |
| S §6, S §10 allowed states | admission: counter replaced but no job directory (gap); finalization: before/after the fact commit; publication with a non-current epoch (no fact); new epoch only without live leases | single-workspace features |
| S §5.1 `cpf init` | creates `.cpforge/job-seq` (fresh epoch, no-clobber) only when `cpforge.toml` was absent at start, before publishing `cpforge.toml`; a rerun **on an existing workspace** never creates it; an interrupted initial `init` keeps an existing `job-seq`; recovery only via `fsck` without live leases | single-workspace features |
| S §7 frozen inputs | solution-target sources copied to `inputs/src/` at admission; every `{sources}`/`{source}` expansion names the copy; accident guard and capture check (§3.2), no sandbox | single-workspace features |
| S §6 finalization | run-fact publication before lease release; failure rule of §3.2 | single-workspace features |
| S §3 / facts schema | fact file format, ordering by `(epoch, n)`, fingerprint encoding of §3.2 | single-workspace features |
| C (workspace config) | `[workspace] ignore = [globs]` with the glob subset and matching rules | single-workspace features |
| S §5 commands | `adopt`, `clean` (= §6.3 with zero budget), `fsck`, `gitignore --write` transactions | single-workspace features |
| S §2, C §1, design §4.1 | views exception, registry, identity | later version (§6) |
| S §3 | retained originals store | deferred (§6.4) |
