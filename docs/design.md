# cpforge: design v2

Status: v2 draft. Name **cpforge**, command `cpf`.

## 1. Goal and non-goals

A self-contained, cross-platform Rust CLI (and later a TUI) that manages the
loop of solving contest problems: fetch problems and their sample tests, keep
them on disk, build and run solutions against them under limits, check the
results honestly, stress-test, switch between problems and contests, and
prepare the files to submit.

The CLI foundation includes a minimal `cpf doctor` (config validation, toolchain
discovery, which limits and measurements this machine can enforce).

"Self-contained" means **fetching and orchestration need
only `cpf`**: no browser, browser extension, Python or helper daemon. Users
still install their compilers and whatever tools their own configured commands
call.

Non-goals: automatic submission, storing judge passwords, executing untrusted
code safely (the runner supervises the user's own programs; it is not a
sandbox), redistributing statements, judge-equivalent timing.

## 2. Principles

- **Plain files, explicit ownership.** Everything is ordinary files a user can
  edit. Downloaded content and user content are distinguishable, and the tool
  never silently overwrites user content (§4.3).
- **Honest results.** A report names what was run, how it was compared, and
  what was measured and how. Anything not checked is *unchecked*, never OK
  (§6).
- **Config for ordinary workflows.** Ordinary build/run workflows are pure
  configuration; specialised language or judge integrations may need code. The claim is tested early with an interpreted language and a language
  whose output is not one native executable.
- **One job engine.** Every operation that runs processes is a cancellable job
  with structured events and a final report; CLI and TUI both consume it.
- **Polite, conservative fetching.** Per-host limits, `Retry-After`, no
  retrying through challenges, throttling shared across concurrent `cpf`
  processes. Courtesy policy, not a claim of permission.

## 3. Delivery milestones

1. **CLI foundation.** Manual problems and tests, C++ build and run, reliable
   cancellation and timeouts, output caps, comparators, structured reports,
   safe file updates, `cpf` config and custom commands (`cpf cmd <name>`).
2. **First useful release.** AtCoder and CSES adapters (contest enumeration
   where the site has it), simple and testlib checkers, **interactive
   problems with adaptive interactors** (protocol in
   `design-checkers.md`), deterministic stress
   with replay, submission artifacts (source copy; optional `distill`),
   tags/status/notes with search, and conservative archive snapshots with
   quarantine and restore (§7a), if their crash and conflict tests pass.
3. **TUI** over the same job API: problem navigation, tests, bounded logs,
   diff, cancellation, stress status.
4. **Incremental adapters**, each when its access and parser coverage are
   demonstrated with fixtures: Timus, Szkopuł (HTML statements; PDFs manual),
   UOJ, QOJ (needs multi-file and scored problems), DMOJ (samples from HTML; its
   API gives metadata only), Library Checker (samples only once their source
   is settled, §7), Codeforces (best-effort public HTML, manual fallback),
   Project Euler (answer problems, §5.4).
5. **Separate later projects:** full test generation,
   editor RPC and clients, hardened isolation, automatic submission.

## 4. Workspace

### 4.1 Identity

Separate records, all TOML:

- **Problem:** canonical identity `<site>:<site-native id>` (e.g.
  `cses:1068`, `atcoder:abc400_a`, `timus:1000`), source URL, title, published
  limits, kind (batch, answer-only, interactive, multi-file; unsupported kinds
  are recorded and refused, not guessed).
- **Contest membership:** contest id, label (`A`), order, contextual
  overrides (limits can differ per contest).
- **Solution target:** language, source files, build profile.
- **Tests:** explicit entries with provenance (§4.2).

Directory names are derived from sanitised identifiers, never from titles or
unchecked remote strings; case-insensitive collisions are detected.

```
<workspace>/
  cpforge.toml                     # workspace config, schema version
  .cpforge/
    state.toml                  # current selection, history
    lock                        # workspace lock for mutating operations
    cache/                      # HTTP cache (conditional requests); deletable
    runs/<job-id>/              # per-run work dirs, outputs, reports (pruned)
  problems/cses-1068/
    problem.toml                # identity, url, limits, kind, checker
    tests.toml                  # explicit test list (§4.2)
    tests/ sample-1.in sample-1.ans custom-1.in
    main.cpp                    # solution (from the language template)
    stress/ gen.cpp brute.cpp   # optional
  contests/atcoder-abc400/contest.toml   # membership: labels -> problem ids
```

Problems live once under `problems/`; contests reference them, so a problem in
several contests is one directory.

### 4.2 Tests

`tests.toml` lists each test: input path, optional answer path, origin
(`sample`, `custom`, `stress`, `imported`), enabled flag, per-test checker
override, and for downloaded tests the content hash at download time.
Filename discovery (`*.in` with sibling `*.ans`) is read-only: such files
are listed as unregistered and become tests only through `cpf test register`
(storage contract §8.1), so a half-finished `test add` never turns into a
runnable test; an absent answer and an empty answer are different. `.ans` is the
reference answer, `.out` a produced output.

### 4.3 Fetch and update safety

Fetch downloads into a temporary location, validates, then updates
atomically. It never overwrites edited samples, custom tests or solutions: a
downloaded file whose current hash differs from its recorded download hash is
a local edit and is kept, with the new upstream version reported. A refetch
that finds zero samples never deletes an existing test set. Mutating
operations take the workspace lock; files carry a schema version. Updates of
several files (samples plus `tests.toml`) use generation-based publication:
write a new generation directory, fsync, then switch one pointer file; recovery
on start finishes or discards incomplete generations. The guarantee covers
process crashes; power-loss durability is best-effort per filesystem.
Upstream sample renumbering is reported as a conflict, not auto-mapped; last
successful metadata is kept apart from the latest fetch status.

### 4.4 Current selection

The "staging area" is a default selection, not authority over running jobs.
A job resolves its problem once at start: `--problem` → the problem containing
the current directory → the saved current selection. `cpf current --path`
is the stable interface for shells and editors; an optional `current`
symlink/junction is off by default.

## 5. Configuration and commands

### 5.1 Layers

User config (`~/.config/cpforge/config.toml`; `%APPDATA%\cpforge\config.toml`) →
workspace `cpforge.toml` → problem keys. Arrays replace on merge; inherited hooks
can be disabled explicitly. Unknown keys and unknown placeholders are errors
before anything runs. Relative paths resolve against the file that defines
them; platform overrides (`[languages.cpp.windows]`); environment is
inherited minus an explicit `env_remove`, plus `env`. Per-problem language
selection is part of the core target model.

### 5.2 Languages and execution specs

```toml
[languages.cpp]
extensions = ["cpp", "cc"]
template = { path = "~/cp/template.cpp" }        # ~ expanded only in path fields
sources = ["main.cpp"]
[languages.cpp.build]
argv = ["g++", "-std=gnu++23", "-O2", "-Wall", "{sources}", "-o", "{artifact}"]
deadline_s = 60
[languages.cpp.run]
argv = ["{artifact}"]
[languages.cpp.profiles.debug.build]
argv = ["g++", "-std=gnu++23", "-g", "-fsanitize=address,undefined", "{sources}", "-o", "{artifact}"]
[languages.cpp.submit]
argv = ["distill", "--judge", "{judge}", "-o", "{submit_file}", "g++", "{sources}"]
[languages.python.run]                       # interpreted: no build step
argv = ["python3", "{sources}"]
```

An execution spec has `argv`, `cwd`, `env`, stdin/stdout routing, a
deadline, and declared artifacts. No shell unless `shell = { interpreter =
["bash", "-c"] }` is given explicitly. `{sources}` expands to several
arguments and is only allowed as a whole argv element; literal braces are
`{{`/`}}`. The CLI foundation must demonstrate a configuration-only multi-artifact
workflow (Java: several class files, entry point) besides Python.

### 5.3 Custom commands and hooks

`cpf cmd <name>` runs `[commands.<name>]` (namespaced so new built-ins cannot
collide). Optional `for_each_test = true` with first-class stdin routing from
the test input, and a built-in `repeat = N` for simple benchmarking instead of
delegating to other tools' shell parsing. Hooks (`on_switch`, `after_test`, …)
are opt-in; reading config, listing and fetching never execute hooks, since
workspace config is executable code.

### 5.4 Answer-only problems (Project Euler)

An answer artifact (text, exact string as entered by the user) plus optional
downloaded data files. No public expected answers; `cpf check` compares
against a user-recorded answer only if one exists, otherwise reports
unchecked.

## 6. Running and checking

### 6.1 Result model

Each run records separately:

- **Execution:** exited(code) / signalled / wall deadline exceeded / output
  limit exceeded / cancelled / spawn failed.
- **Checking:** passed / failed / unchecked / checker failed / unsupported.
- **Measurements:** each with metric name, unit, scope and method (for
  example "peak RSS via wait4, process tree: no"); unsupported metrics are
  `null` with a reason.
- **Evidence:** comparator and its settings, checker identity, test origin,
  build identity.

Summaries may use familiar labels (OK, WA, TLE, RE), but never claim more
than the record: no answer and no checker is *unchecked*; no tests is not a
pass; a killed process is not automatically MLE; a checker crash is not WA;
"passed 3 samples (token comparison)" never becomes "accepted". Published
judge limits and local enforcement limits are separate fields.

### 6.2 Supervisor

- Wall-clock deadline always. Unix: process group (and a cgroup when
  delegated) killed on timeout/cancellation, with a bounded pipe-drain
  shutdown; descendants that leave the group (setsid, daemonise) are a
  documented limitation. Windows: the process is created inside its Job
  Object (no spawn-then-assign race), kill-on-close. Inherited pipe handles
  cannot keep a run alive past the drain deadline.
- stdout/stderr drained concurrently and strictly capped (compiler and
  checker output too); retained run artifacts are capped; files a program
  writes itself are only monitored best-effort (no quota promise).
- Separate deadlines for build, run, checker, generator, brute and submit
  preparation; separate work directory per run (file-based I/O safe).
- Memory: Linux reports peak RSS (`wait4`) and enforces with `RLIMIT_AS` only
  when asked (address space, documented as such); delegated cgroup v2 used
  when available for tree accounting (`memory.peak`, named as cgroup memory).
  Windows reports peak working set and enforces Job Object commit limits,
  named as such. macOS reports a named, validated metric; enforcement limited.
  `cpf doctor` prints what this machine supports.
- Tests run sequentially by default (timing-sensitive); `--jobs N` is an
  explicit throughput mode. Sanitizer profiles are diagnostic.

### 6.3 Comparators

`tokens` (default; ASCII whitespace, CRLF = LF, invalid bytes compared
exactly), `exact` (bytes), `lines` (trailing whitespace per line ignored),
`float { abs, rel }` (documented formula `|a-b| <= max(abs, rel*|b|)`, token
counts must match, integer tokens compared as integers, NaN/inf/non-numeric
defined in a table in the implementation spec before the CLI foundation is complete: mixed
integer/decimal tokens, overflow, signed zero, huge integers). Samples are
stored as produced by a documented extraction algorithm (HTML entity
decoding, line endings); nothing is trimmed afterwards; raw response
provenance kept separately.

Aggregate result precedence: spawn failure > cancellation > wall deadline >
output limit > signal > nonzero exit > checker failure > wrong answer >
unchecked > pass. Checking still runs after an unsuccessful execution for
diagnostics, but a matching partial output never yields success. A requested
limit the platform cannot enforce is an error, never silently ignored.

### 6.4 Checkers

- **Simple protocol:** `checker <input> <output> [<answer>]`, exit 0 = pass,
  1 = wrong answer, anything else (other codes, signals, timeout) = checker
  failure; stderr kept, bounded.
- **testlib:** separate adapter pinned to testlib's default exit-code
  convention (`checker in out ans`); raw result kept; partial scores and
  unknown codes are reported as unsupported, never mapped to pass/fail.

### 6.5 Stress

`cpf stress` with an explicit seed contract (`gen <seed>`), start seed,
iteration count and wall-time budget; optional input validator; independent
results for generator, oracle (brute), candidate and checker. On a mismatch it
saves input, both outputs, stderr, seed, command lines and build identities,
saved failures are promoted out of prunable run directories into the
problem's `stress/` (they are user work); `cpf stress --replay <id>` reruns
the saved input against current sources, `--replay-binaries` against the
preserved executables. Generator, oracle and checker failures are saved too.
Replay is not a determinism promise for nondeterministic programs. The brute's output becomes an
`.ans` only after exit code 0, complete capture and successful validation; for constructive problems a checker
decides validity instead of comparing outputs.

### 6.6 Builds

Every `test` rebuilds by default (an unsound cache is worse than none).
Later: a cache keyed by command, profile, toolchain identity, relevant
environment and compiler dependency files. A failed build never runs a stale
binary.

### 6.7 Submission artifacts

`cpf submit-prep` has a plain source-copy mode independent of any tool;
the `distill` step (cpp-distill, pinned by version) is an optional language
setting. It preserves the source, writes the submission artifact(s)
separately (a set, not one file), builds and tests the artifact in a clean
directory (so headers from the workspace cannot leak in) when the language
allows, and prints its path and the submit URL. Copying to the
clipboard is opt-in and failure there never fails the command.

## 7. Site adapters

Interface: `resolve(url) -> ProblemRef/ContestRef`, `fetch_problem`,
`fetch_contest`, each returning a result with data, per-field availability,
attachments, provenance (URL, time, HTTP status, content hash) and warnings,
or a classified failure: authentication required, challenge/rate limit,
unsupported task format, parser failure, valid problem without samples.
Capabilities are per response, not per site. Scraped data never becomes an
executable command or trusted checker path.

Initial feature release adapters:

- **AtCoder:** pick one statement language section (pages carry Japanese and
  English samples; a naive extractor duplicates tests), pair numbered samples
  within it, parse limits; refuse unsupported task kinds.
- **CSES:** public problem set first.

Notes for later adapters: decode by declared charset with tested fallbacks
(not hard-coded Windows-1251 for Timus); DMOJ samples come from HTML, its API
is metadata-only and rate-limited (90/min, CAPTCHA risk); Library Checker
samples need a settled source because generated `in/`/`out/` are not in the
repository; Codeforces' API has no statements or samples (1 request/2 s) and
statement pages may be blocked: best-effort HTML plus manual fallback, no
mandatory cookie import; UOJ, QOJ and Universal Cup get separate
host-specific adapters sharing utilities only where fixtures show equivalence;
Szkopuł has HTML statements (PDF statements are manual entry).

Legal: local caching only; fixtures committed to the repository are minimal
excerpts with provenance, reviewed per site (AtCoder reserves rights;
Project Euler content is CC BY-NC-SA 4.0; Library Checker is Apache-2.0;
Codeforces restricts commercial use).

## 7a. Archiving, tags and organisation

### Archive (initial feature release: conservative snapshots)

`cpf archive <problem|contest>` writes an **immutable archive snapshot** and
then retires the active copy. The user decides what the snapshot keeps and how
archives are laid out; the tool guarantees that nothing is lost by accident.

**Safety rules**
- Planning is always non-destructive; changes need `--apply`. A plan binds to
  the file inventory, configuration, destination and references; any change
  invalidates it.
- Only **tool-owned disposable** content (build outputs, run directories,
  caches) is discarded automatically. Every other file is preserved unless an
  explicit `discard` rule names it; files no rule mentions (e.g. `helpers.hpp`,
  `alt.cpp`) are kept, and the plan lists them. Locally edited downloaded tests
  and saved stress failures count as user content.
- Identity, tags, status, solution targets and the metadata that interprets
  tests (`tests.toml`) are always kept, whatever the keep profile says.
- Transaction: inventory and plan → stage on the destination filesystem (no
  cross-mount rename) → verify and fsync → publish the snapshot (manifest last)
  → revalidate the source → apply the configured **cleanup mode** (§11):
  `trash` (OS trash, proposed default), `quarantine` (`.cpforge/quarantine/`,
  purged by `cpf quarantine purge` or after `retention_days`), `delete`
  (irreversible, explicit only), or `keep` (copy-only export). Recovery
  tolerates "snapshot committed, source cleanup incomplete"; duplicates are
  preferred over loss. Editors can still save during the operation: a recoverable cleanup mode,
  not a last-second hash, is what protects that work; `delete` says so in the
  plan.
- Fixed lock order (workspace, then archive root); refuses while jobs on the
  problem are running; rejects source/archive overlap and symlink/junction
  traversal; disk-full, permission and cleanup failures are reported
  separately from snapshot success. Network or cloud-synced archive roots are
  outside the initial durability guarantee.

**What to keep:** a base policy plus at most one named profile chosen
explicitly (`cpf archive --keep-profile minimal`). Overlapping automatic
status/tag rules are deferred (they need ordered first-match semantics).

```toml
[archive]
roots = ["~/cp-archive"]                    # all known roots, searched by cpf
default_root = "~/cp-archive"
layout = "{site}/{problem_id}"              # unique leaf {snapshot_id} always appended
keep = ["sources", "tests:custom", "notes"] # classes; metadata always kept
discard = ["tests:sample"]                  # explicit; unlisted files are kept
[archive.profiles.minimal]
keep = ["sources"]
discard = ["tests:sample", "tests:custom", "stress"]
```

"sources" means the solution target's declared files plus every file with a
configured language extension in the problem directory.

**Layouts:** typed scalar placeholders only (`{site}`, `{problem_id}`,
`{contest_id}`, `{label}`, `{contest_date_year}`); each expands to exactly one
sanitised path component; a missing value is an error, never an empty string;
`{snapshot_id}` is always the leaf. Expanded values are frozen in the
manifest, so later tag or title changes never move an archive. Validation
covers Windows reserved names and trailing dots/spaces, separators, drive/UNC
forms, case and Unicode-normalisation collisions, length limits, and names
inside the payload, all before anything is published. `--to <dir>` overrides
the root for one operation. Title-derived slugs and `{tags}` are not layout
fields.

**Contests:** a contest snapshot stores `contest.toml` (labels, order, limit
overrides) and references problem snapshot IDs; it is complete only when every
member snapshot is committed. A problem still referenced by another active
contest is snapshotted but not retired; retiring it is a separate explicit
operation.

**Conflicts and restore:** snapshots are immutable with unique IDs; default
conflict behaviour is an error; `reuse-identical` only after a full
equivalence check (inventory, bytes, metadata, identity); no merging.
`cpf restore` restores *retained content*, not a reversal of a lossy archive:
it copies into staging, validates, then commits without consuming the
snapshot; an existing active problem with the same identity is a conflict
(`--into <dir>` restores beside it). The manifest records schema and snapshot
IDs, file inventory with hashes, sizes and executable bit, problem and
membership metadata, solution targets, test definitions, relevant effective
config as data, explicit omissions, and unresolved external dependencies
(templates, library paths, toolchains), which restore reports. Restore and
search never execute hooks; manifest paths are validated as untrusted input.

Deferred: permanent selective deletion without quarantine, overlapping rules,
archive merging/relocation, cross-machine sync.

### Tags, status and notes

- **Tags** on problems and contests (`cpf tag cses:1068 dp graphs`,
  `cpf untag …`); stored in `problem.toml` / `contest.toml`, so they travel
  into the archive.
- **Status** per problem is user-maintained workflow state (`new`,
  `in-progress`, `solved`, `skipped`, user-defined); passing samples never
  sets `solved`. `upsolve` is a tag, so it can coexist with any status.
  Contest tags stay separate from problem tags. Dates are named fields
  (`created_at`, `updated_at`, `solved_at`, `archived_at`); `cpf status` and `cpf ls` filter by
  status, tag, site, contest and date.
- **Notes:** an optional `notes.md` per problem (ideas, editorial links),
  kept by archive rules like any other class.
- `cpf search` / `cpf ls --archived` scan workspace metadata and archive
  manifests (and bounded notes files) directly, without a database; results
  say whether they are active problems or snapshots, and unreadable roots or
  malformed manifests are reported, never hidden behind "no matches".

### Feature backlog (to prioritise later)

Collected so nothing is lost; each item needs its own design before work:
contest timer and status board; per-problem time tracking; solution
snippets/library insertion; templates per site or tag; multiple solutions per
problem (e.g. `main.cpp`, `brute.cpp`, `alt.cpp`) with a selectable target;
per-problem language override; statistics (solved per tag/site/month);
import/export of an existing directory tree; watch mode (rerun tests on
save); diff viewer for failing tests; clean command for build/run outputs;
shell completions; `cpf doctor` (toolchains, limits support, config
validity).

## 8. Machine-readable interface

`--json` produces a versioned report (one JSON document) or `--events` a JSONL
event stream; diagnostics on stderr; child output goes to run artifacts or
encoded events, so raw `cpf run` passthrough stays separate. Stable,
documented exit codes. The later editor RPC wraps the same job API.

## 9. Architecture

Start with one library crate (modules: `model`, `workspace`, `config`,
`jobs`, `supervisor`, `compare`, `check`, `stress`, `sites`, `http`,
`report`) and one binary crate; split crates when dependency or platform
boundaries justify it. The TUI is a separate binary module over
`jobs`. Dependencies kept modest: clap, serde/toml, reqwest+rustls, scraper,
tokio or a small thread-based job runner (decide in the CLI foundation), ratatui later.

## 10. Testing the tool

Archive/fetch: crash injection at every commit boundary, edit during copy,
concurrent destination creation, shared-contest problems, cross-filesystem
moves, disk full, Windows sharing violations, symlinks/junctions, restore
conflicts. Runner: descendants, inherited pipes, huge output, slow and crashing
checkers, stale binaries, paths with spaces and Unicode, cancellation,
concurrent workspace operations. Fetch: interrupted downloads, edited samples
during refetch, zero-sample refetch, collisions. Adapters: saved fixtures with
real structural variants (both statement languages, unusual tasks) and
synthetic edge cases (missing, duplicated, challenge, login pages); opt-in live
smoke tests run on a schedule in CI. CI on Linux, macOS and Windows.

## 11. Decisions and open questions

Design decisions:

- Initial feature release sites: **AtCoder and CSES** (public batch tasks).
- Runtime: **Tokio**, with a small platform supervisor abstraction; the
  CLI foundation spike validates process lifecycle.
- Archive cleanup is configurable (`cleanup = "quarantine" | "trash" |
  "delete" | "keep"`, per run and per profile; `retention_days` for
  quarantine). Proposed default: `trash` (OS trash: macOS Trash, Windows
  Recycle Bin, freedesktop trash), `quarantine` where no OS trash exists,
  `delete` only explicitly with an irreversible-warning in the plan. All modes
  run after the snapshot is verified and published.

Additional decisions: name **cpforge**, command `cpf`; default
cleanup mode `trash`; float comparison methods configurable, integer answers
strict by default (`compare-spec.md`); custom checkers and interactive
problems with adaptive interactors ship in the initial feature release, with the two-process
mechanics (connected runs, relay) built in the CLI foundation's supervisor phase. No open questions are listed here.
