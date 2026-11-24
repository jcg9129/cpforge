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
2. **First useful release.** AtCoder and CSES adapters (AtCoder contest
   enumeration; the CSES problemset only, CSES contests are refused), simple
   and testlib checkers and validators, scores, **interactive problems with
   adaptive interactors** (protocol in `design-checkers.md`), file-based
   I/O for batch problems, seeded stress with saved failures and replay,
   submission artifacts (source copy; an optional generic `submit` stage
   such as `distill`), tags/status/notes with search, and conservative
   archive snapshots with quarantine and restore (§7a), if their crash and
   conflict tests pass. Operations that start processes (judging, stress,
   the `submit` stage) run on Linux and macOS and are refused on Windows
   until the Windows supervisor exists; fetching, organisation, source-copy
   submission and archiving need no process and work on all three. A
   draft pre-release comes first, once fetching and batch checkers work on
   Linux and macOS; its report schema already holds every variant of the
   release, so later parts only fill them in. Not in this milestone, each
   refused explicitly when asked for: `--jobs N`, a build cache, cgroup
   accounting, subtasks, output-only and multi-case problem kinds, stress
   shrinking, byte-stream replay and per-test checker overrides.
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
    fetch/<problem>/            # fetch journal, history and status (§4.3)
    submit/<problem>/           # submission versions and `current` (§6.7)
    archive/ quarantine/        # archive transactions, displaced files (§7a)
  problems/cses-1068/
    problem.toml                # identity, url, limits, kind, checker, tags, status
    tests.toml                  # explicit test list (§4.2)
    tests/ sample-1.in sample-1.ans custom-1.in
    main.cpp                    # solution (from the language template)
    notes.md                    # optional (§7a)
    stress/ gen.cpp brute.cpp   # optional; stress/failures/ holds saved failures
  contests/atcoder-abc400/contest.toml   # membership: labels -> problem ids
```

Problems live once under `problems/`; contests reference them, so a problem in
several contests is one directory. The complete list of tool-owned trees is
in `design-workspace.md` §2.1.

### 4.2 Tests

`tests.toml` lists each test: input path, optional answer path, origin
(`sample`, `custom`, `stress`, `imported`), enabled flag, and for
downloaded tests a `download` record (the input and answer hashes at
download time, the upstream ordinal and the fetch transaction that
published them; storage contract §8.1). Per-test checker overrides are not
supported: judging is a problem-level setting, and an override is refused
rather than ignored.
Filename discovery (`*.in` with sibling `*.ans`) is read-only: such files
are listed as unregistered and become tests only through `cpf test register`
(storage contract §8.1), so a half-finished `test add` never turns into a
runnable test; an absent answer and an empty answer are different. `.ans` is the
reference answer, `.out` a produced output.

### 4.3 Fetch and update safety

Fetch never modifies or deletes a test file. Test files are ordinary files
that users edit, and editors do not honour cpf's lock, so the only safe
update is to publish new files beside the old ones and change which files
the catalog names. An update is therefore a **catalog commit with a fetch
journal** (storage contract §5.8), not a switch between generation
directories:

- The network phase runs without the workspace lock, into a leased
  download area, and records what it saw (catalog, file hashes,
  `problem.toml`). Under the lock, fetch revalidates that base; this is the
  point where a local edit is observed.
- It writes an intent journal, publishes the new files no-clobber, commits
  the `tests.toml` edit, then the `problem.toml` tool fields, then a
  history record and the status. A crash leaves a journal that recovery
  rolls forward or abandons; readers meanwhile use the committed catalog
  and report the fetch as incomplete.
- A sample is an (input, answer) pair. An unedited pair whose upstream
  changed is published under revision names and replaces the catalog
  entry; the old files stay as superseded downloads. An edited pair (either
  file) is kept, and the upstream pair is published beside it as an
  unregistered revision pair and reported. A user-disabled sample stays
  disabled. Matching is conservative: renumbering, permutations or any
  ambiguity is a conflict, and nothing is registered. A refetch with zero
  samples keeps the existing set; a sample removed upstream is reported
  and disabled only on request.
- Each committed fetch leaves a history record listing every file it
  published with its hash and role. The record, not a file name, decides
  whether a file is a tool download or user content (`design-workspace.md`
  §2.2), and it travels with the problem into an archive, so the upstream
  version of a conflicting sample never depends on the disposable HTTP
  cache.

The remaining race is stated rather than hidden: an editor save between
fetch's last rehash and the catalog rename is not detected. No file is
modified or deleted, but the catalog then names the upstream revision and
`status` reports the old pair as edited. The guarantee covers process
crashes; power-loss durability is best-effort per filesystem. Last
successful metadata is kept apart from the latest fetch status. Mutating
operations take the workspace lock; files carry a schema version.

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
version = { argv = ["distill", "--version"], expect = "^distill 1\\." }   # optional; pins the tool
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

- Wall-clock deadline always. Unix: process group (later also a cgroup
  when delegated) killed on timeout/cancellation, with a bounded pipe-drain
  shutdown; descendants that leave the group (setsid, daemonise) are a
  documented limitation. Windows: the process is created inside its Job
  Object (no spawn-then-assign race), kill-on-close. Inherited pipe handles
  cannot keep a run alive past the drain deadline.
- stdout/stderr drained concurrently and strictly capped (compiler and
  checker output too); retained run artifacts are capped; files a program
  writes itself are only monitored best-effort (no quota promise). The one
  exception is the declared output file of a file-based I/O problem, which
  cpf reads back under the stdout cap (a read bound, not a write quota;
  execution contract §5.6).
- Separate deadlines for build, run, checker, generator, brute and submit
  preparation; separate work directory per run (file-based I/O safe: the
  declared input is staged into the solution run's own directory and the
  output file collected from it; a shared working directory with file-based
  I/O is a configuration error).
- Memory: Linux reports peak RSS (`wait4`) and enforces with `RLIMIT_AS` only
  when asked (address space, documented as such); a later version uses
  delegated cgroup v2 when available for tree accounting (`memory.peak`,
  named as cgroup memory).
  Windows reports peak working set and enforces Job Object commit limits,
  named as such. macOS reports a named, validated metric; enforcement limited.
  `cpf doctor` prints what this machine supports.
- Tests run sequentially (timing-sensitive). A later `--jobs N` would be an
  explicit throughput mode; until then it is refused, as is cgroup
  accounting. Sanitizer profiles are diagnostic.

### 6.3 Comparators

`tokens` (default; ASCII whitespace, CRLF = LF, invalid bytes compared
exactly), `exact` (bytes), `lines` (trailing whitespace per line ignored),
`float { abs, rel }` (documented formula `|a-b| <= max(abs, rel*|b|)`, token
counts must match, integer tokens compared as integers, NaN/inf/non-numeric
defined in a table in the implementation spec before the CLI foundation is complete: mixed
integer/decimal tokens, overflow, signed zero, huge integers). Samples are
stored as produced by a documented extraction algorithm (sites contract §7:
UTF-8 only, HTML5 entity decoding, `<br>` and `<pre>` rules, line endings,
NBSP, and one LF appended to a non-empty sample that lacks a final LF);
nothing is trimmed afterwards, so leading and trailing blank lines and
spaces stay. Raw response provenance (URL, time, status, body hash) is
kept separately, in the fetch report and the disposable cache.

Aggregate result precedence puts any failure of cpf's own observation
first (infrastructure: a process that could not start, a supervisor error,
incomplete cleanup, lost capture), then cancellation, then the program's
own failures before checker failures, wrong answers, unchecked and pass,
so a result is never better than what was observed. The order and its
derivation are the execution contract §6.4 and the report contract §§2.7,
2.11–2.12. Checking still runs after an unsuccessful execution for
diagnostics, but a matching partial output never yields success. A requested
limit the platform cannot enforce is an error, never silently ignored.

### 6.4 Checkers

Checkers, validators and interactors are specified in `design-checkers.md`,
which supersedes the sketch this section used to hold; the configuration
contract §9.1 owns `[check]`. In outline:

- A judge is the user's own program, built from source inside the job or
  given as a prebuilt path (then its provenance is `declared`), never taken
  from fetched data.
- **`simple`:** `checker <input> <output> <answer>`, exit 0 = pass, 1 =
  wrong answer, anything else is a checker failure.
- **`testlib`:** the checker always receives a report path and `-appes`,
  and cpf decides from the exit code and the XML report together, for one
  pinned, tested header. Presentation errors stay distinct from wrong
  answers. Points are supported when the problem enables scoring, with
  `sum` or `min` aggregation; partial-correct codes and anything outside the
  profile are a checker failure, never mapped to pass or fail.
- Whether a test may be judged without an answer is a problem-level policy
  (`answer = "file" | "none"`), so an absent answer is never silently
  replaced by an empty one.

A checker failure is never a wrong answer or a zero score. The judges that
decided a result are part of the run fact, so `status` reports a checker
edit instead of calling the result unchanged.

### 6.5 Stress

`cpf stress` with an explicit seed contract (`gen <seed>`, seed = start +
iteration, never wrapping), a start seed (printed when drawn at random),
an iteration count and a wall-time budget (defaults 10 000 iterations or
60 s); optional input validator; independent results for generator,
validator, oracle (brute), judge and candidate. An iteration is classified
once, and only a candidate failure is reported as a bug in the solution.
On a failing iteration it saves input, outputs, stderr, seeds, command lines
and build identities; saved failures are promoted out of prunable run
directories into the problem's `stress/failures/` (they are user work,
never pruned). `cpf stress --replay <id>` reruns the saved input against
current sources, `--replay-binaries` against the preserved executables
(same OS and architecture), and `cpf test add --from-stress` turns a saved
failure into a test. Generator, validator, oracle and judge failures are
failing iterations and saved like candidate failures, within the same caps:
without `--keep-going` the first failing iteration stops the job; with it,
at most 20 failures per job are saved (and at most 256 MiB of preserved
programs, 64 MiB each), and later ones are only counted. An iteration that
ends in an infrastructure failure or a cancellation is never saved and
stops the job, because an observation cpf could not make reliably once is
likely to repeat. Replay is not a determinism promise for nondeterministic
programs. The brute's output becomes the iteration's answer only after exit
code 0, complete capture and successful validation; for constructive
problems a checker decides validity instead of comparing outputs.

A stress job keeps one live iteration and releases its resources before the
next is admitted, so its memory, files and slots stay bounded however many
iterations it runs; only counters grow. The exact lifecycle, outcome classes
and the saved-failure format are the stress contract. Shrinking and stress
of file-based I/O problems are not in this version.

### 6.6 Builds

Every `test` rebuilds by default (an unsound cache is worse than none).
Later: a cache keyed by command, profile, toolchain identity, relevant
environment and compiler dependency files. A failed build never runs a stale
binary.

### 6.7 Submission artifacts

`cpf submit-prep` has a plain source-copy mode independent of any tool, and
a generic `submit` stage configured per language (for example cpp-distill)
as an option. It preserves the source and writes the submission artifact(s)
separately (a set, not one file), as an immutable version under
`.cpforge/submit/<problem>/` made current by one reference; `cpf status`
calls a submission stale when its sources changed, and `cpf clean` removes
versions that are no longer current. It prints the artifact's path and, for
a known site, the submit URL. Copying to the clipboard is opt-in
(`--copy`), and failure there never fails the command. The submission
contract has the exact rules; three points narrow the original promises:

- **Source copy is untested.** The copied files are the ones `cpf test`
  judged, so source copy builds and runs nothing and the version says
  `tested = false`; `status` shows the solution's own test result next to
  it.
- **The tool is pinned only when the stage declares a version probe.**
  Without one the version records the tool as `unpinned` and the report
  warns. A generic stage cannot know how an arbitrary tool reports its
  version.
- **A stage's artifact is tested by default, not always.** `test = false`
  in the submit table (for a tool whose output the target's stages cannot
  build) or `--no-test` skips the artifact test, and the version then says
  `tested = false` (submission contract §3.2).
- **The artifact test is isolated by location, not by observation.** When
  it runs, a stage's artifact is built and tested in a fresh directory outside the
  workspace holding only the artifact and the files it is provided with,
  with the accident guard on its operands. An absolute `#include` of a
  workspace file, an include path in the inherited environment or a
  wrapper is not detected, because cpf does not trace a compiler's file
  accesses.

The `submit` stage starts processes and is therefore refused on Windows for
now; source copy works on every platform.

## 7. Site adapters

Interface: `resolve(url) -> ProblemRef/ContestRef`, `fetch_problem`,
`fetch_contest`, each returning a result with data, per-field availability,
provenance (URL, time, HTTP status, content hash) and warnings, or a
classified failure. The classes (sites contract §8.3) keep apart what the
user must do differently: authentication required, challenge, rate limited
(with a retry time), not found, not available yet, unsupported, an
unrecognized page, parser failure, network, and an offline cache miss. A
recognized page without samples is a warning, not a failure. Capabilities
are per response, not per site. An adapter accepts a page only when it
positively recognizes it as a task page by its site-specific markers;
anything else (a login or challenge page, a redesign) is classified and
publishes nothing, which fails safe at the cost of blocking until the
adapter is fixed. Scraped data never becomes an executable command, a
trusted checker or interactor path, a comparator or any other judging
setting: a tolerance statement on a page is reported as a hint only.
Attachments are not fetched in this version.

HTTP goes through one cpf dispatcher that performs every redirect, retry
and conditional request itself under a persistent per-host permit shared by
all `cpf` processes (spacing AtCoder 2 s, CSES 1 s, one request at a time
per host), honours `Retry-After`, persists cooldowns, never retries a
challenge, and bounds each invocation (200 requests, 15 minutes of network
time). A bulk selection is enumerated with one request and refused before
any member request above 50 problems. The cache is local and disposable;
`--offline` reads only the cache. The numbers and the outcome table are the
sites contract.

Initial feature release adapters:

- **AtCoder:** pick one statement language section (pages carry Japanese and
  English samples; a naive extractor duplicates tests): English, else
  Japanese with a warning, configurable. Pair numbered samples within it,
  parse limits, detect interactive tasks (no interactor is fetched); refuse
  unsupported task kinds. Contests are enumerated from their task list.
- **CSES:** the public problemset, one problem or a whole section. CSES
  contests are refused as unsupported.

Notes for later adapters: decode by declared charset with tested fallbacks
(not hard-coded Windows-1251 for Timus); DMOJ samples come from HTML, its API
is metadata-only and rate-limited (90/min, CAPTCHA risk); Library Checker
samples need a settled source because generated `in/`/`out/` are not in the
repository; Codeforces' API has no statements or samples (1 request/2 s) and
statement pages may be blocked: best-effort HTML plus manual fallback, no
mandatory cookie import; UOJ, QOJ and Universal Cup get separate
host-specific adapters sharing utilities only where fixtures show equivalence;
Szkopuł has HTML statements (PDF statements are manual entry).

Legal: local caching only. Fixtures in the public repository are synthetic:
minimal markup written from scratch with each site's marker structure and
no site text. Parsers also need real structural variants, so minimal
excerpts of real pages (markup skeleton and sample blocks, prose stripped,
with a provenance file, reviewed per site) are kept outside the public
repository and reach CI only as an encrypted bundle pinned by version and
hash (sites contract §9). Pull requests from forks cannot decrypt it and
skip those cases visibly; every other CI run requires them (AtCoder reserves
rights; Project Euler content is CC BY-NC-SA 4.0; Library Checker is
Apache-2.0; Codeforces restricts commercial use).

## 7a. Archiving, tags and organisation

### Archive (initial feature release: conservative snapshots)

`cpf archive <problem|contest>` writes an **immutable archive snapshot** and
then retires the active copy. The user decides what the snapshot keeps and how
archives are laid out; the tool guarantees that nothing is lost by accident.
The archive contract has the exact plan, crash tables and recovery; this
section gives the shape and the reasons.

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
  cross-mount rename) → verify and fsync → publish the snapshot (manifest last).
  `keep` (copy-only export) is decided here, before anything is retired: it
  stops after the snapshot and changes nothing in the workspace. Every other
  mode then **retires** the problem by one atomic rename into
  `.cpforge/archive/<txn>/retired/` (journaled with the directory's object
  identity, so recovery never acts on a path that now names something
  else), clears the selection and the run facts, and finally **cleans up
  the retired tree only**: each file is revalidated against the snapshot
  hashes, and only matching files go to the configured **cleanup mode**
  (§11): `trash` (OS trash, default), `quarantine` (`.cpforge/quarantine/`,
  removed only by an explicit `cpf quarantine purge`; `retention_days`
  selects what such a purge may remove, nothing is purged automatically) or
  `delete` (irreversible, explicit only). Divergent, new or ambiguous files
  go to quarantine in every mode, and so do files a keep profile discarded
  when the mode is `delete`. Recovery tolerates "snapshot committed, source
  cleanup incomplete"; duplicates are preferred over loss. Editors can
  still save during the operation: a recoverable cleanup mode, not a
  last-second hash, is what protects that work. A file saved at the old
  path after retirement is reported by `status` and `fsck`, never removed.
  Under `delete`, bytes written through a handle opened before retirement
  and after the last check are lost, and the plan says so.
- Fixed lock order (workspace, then archive root); refuses while **any** job
  of the workspace is running (a job records no durable binding to its
  problem yet, so a running stress job blocks every archive) and while a
  fetch of the problem is incomplete; rejects source/archive overlap and
  symlink/junction traversal; disk-full, permission and cleanup failures are
  reported separately from snapshot success. The OS trash is used on every
  platform; a trash that copies instead of renaming, or a disabled or too
  small Windows Recycle Bin, can lose discarded files (never snapshot
  content), and the documentation says so. Network or cloud-synced archive
  roots are outside the initial durability guarantee; cpf warns about a
  known network filesystem.

**What to keep:** a base policy plus at most one named profile chosen
explicitly (`cpf archive --keep-profile minimal`). Overlapping automatic
status/tag rules are deferred (they need ordered first-match semantics).

```toml
[archive]
roots = ["~/cp-archive"]                    # all known roots, searched by cpf
default_root = "~/cp-archive"
layout = "{site}/{problem_id}"              # unique leaf {snapshot_id} always appended
contest_layout = "{site}/contests/{contest_id}"
keep = ["sources", "tests:custom", "notes"] # classes; metadata always kept
discard = ["tests:sample"]                  # explicit; unlisted files are kept
[archive.profiles.minimal]
keep = ["sources"]
discard = ["tests:sample", "tests:custom", "stress"]
```

"sources" means the solution target's declared files plus every file with a
configured language extension in the problem directory. The fetch history
is kept like metadata, since it is what tells downloaded tests from user
content; submission versions are regenerable tool output and are omitted
unless `keep` names `submit`.

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
member snapshot is committed (its manifest is the completion marker, written
last; an incomplete contest snapshot is listed as such and restores only with
`--partial`). A problem still referenced by another active contest is
snapshotted but not retired; retiring it is a separate explicit operation.

**Conflicts and restore:** snapshots are immutable with unique IDs; default
conflict behaviour with an existing snapshot is an error; `new` writes
another snapshot beside it; `reuse-identical` only after a full equivalence
check (inventory, bytes, metadata, identity); no merging.
`cpf restore` restores *retained content*, not a reversal of a lossy archive:
it copies into staging, validates, then commits without consuming the
snapshot; an existing active problem with the same identity is a conflict
(`--into <dir>` restores beside it). The manifest records schema and snapshot
IDs, file inventory with hashes, sizes and executable bit, problem and
membership metadata, solution targets, test definitions, relevant effective
config as data, explicit omissions, and unresolved external dependencies
(templates, library paths, toolchains), which restore reports. Restore and
search never execute hooks; manifest paths are validated as untrusted input,
and writer and reader share one set of limits (files, bytes, path length),
so cpf never writes a snapshot its own restore refuses. Restore checks that
every destination ancestor is a real directory immediately before
publishing; this containment is path-based, so a concurrent swap of a
directory after the final check is outside the guarantee, as for every
workspace path.

Deferred: permanent selective deletion without quarantine, overlapping rules,
archive merging/relocation, cross-machine sync.

### Tags, status and notes

The organisation rules are storage contract §5.9.

- **Tags** on problems and contests (`cpf tag cses:1068 dp graphs`,
  `cpf untag …`); free-form ASCII words compared case-insensitively, stored
  in `problem.toml` / `contest.toml` so they travel into the archive.
- **Status** per problem is user-maintained workflow state set by `cpf
  mark`: built-in `new`, `in-progress`, `solved`, `skipped`, plus values
  configured in `status.values`; a problem never marked counts as `new`.
  Passing samples never sets `solved`. `upsolve` is a tag, so it can
  coexist with any status. Contests have tags only; their status is derived
  from their members' and never stored. Dates are named fields
  (`created_at`, `updated_at`, `solved_at`, `archived_at`), stored in UTC;
  `solved_at` keeps the first solve. `cpf status` and `cpf ls` filter by
  status, tag, site, contest and date with one shared grammar; date
  filters use local calendar days.
- **Notes:** an optional `notes.md` per problem (ideas, editorial links).
  `cpf notes` creates it empty if absent and prints its path; cpf never
  writes it otherwise. Archive rules keep it like any other class.
- `cpf ls --archived` lists snapshots only, from their summaries in the
  archive roots. `cpf search` scans the workspace metadata and notes, then
  (unless `--no-archives`) every snapshot's summary and bounded notes file
  (archive contract §8). Both read files directly, without a database or
  index, and every result says whether it is an active problem or a
  snapshot. A search that reaches its file, byte or traversal bound says
  the result is incomplete, and unreadable roots or malformed summaries
  are reported, never hidden behind "no matches".

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
`jobs`. Dependencies kept modest and pinned: clap, serde/toml, Tokio (chosen
in the CLI foundation), reqwest with rustls (automatic redirects, retries and
decompression off, because cpf's dispatcher does them under its permit and
bounds), scraper, the `trash` crate for the OS trash and `jiff` for local
dates; ratatui later. A `testlib.h` pinned by commit and hash is vendored for
CI only and is not part of a release.

## 10. Testing the tool

Archive/fetch: crash injection at every commit boundary, edit during copy,
concurrent destination creation, shared-contest problems, cross-filesystem
moves, disk full, Windows sharing violations, symlinks/junctions, restore
conflicts. Runner: descendants, inherited pipes, huge output, slow and crashing
checkers, stale binaries, paths with spaces and Unicode, cancellation,
concurrent workspace operations. Fetch: interrupted downloads, edited samples
during refetch, zero-sample refetch, collisions, crash at every journal step.
Adapters: synthetic fixtures in the repository for every marker and edge case
(missing, duplicated, challenge, login pages), and the private excerpt corpus
of real structural variants (both statement languages, unusual tasks)
delivered to CI as described in §7, with a check that every case ran. Live
smoke tests (a few requests, weekly and on demand) are advisory and never
gate a merge. CI on Linux, macOS and Windows; on Windows the operations that
start processes are tested as refusals until the Windows supervisor exists.

## 11. Decisions and open questions

Design decisions:

- Initial feature release sites: **AtCoder and CSES** (public tasks; for
  CSES the problemset only).
- Test publication: **catalog commit with a fetch journal** (§4.3), not
  generation directories, because downloaded tests are editable files.
- Retained originals of downloaded tests (a content-addressed store) are
  deferred: the upstream version of a conflicting sample is kept as an
  unregistered revision pair with a durable history record instead
  (`design-workspace.md` §6.4).
- Report schema 1 is extended in place with checkers, interactors, scores,
  sessions, stress and submission before the first release, so the
  pre-release already carries every variant.
- Runtime: **Tokio**, with a small platform supervisor abstraction; the
  CLI foundation spike validates process lifecycle.
- Archive cleanup is configurable (`cleanup = "trash" | "quarantine" |
  "delete" | "keep"`, per run and per profile). Default: `trash` (OS trash:
  macOS Trash through the in-process file manager call, Windows Recycle Bin,
  freedesktop trash), falling back to `quarantine` when a trash move fails
  after its items were reconciled; `delete` only explicitly with an
  irreversible-warning in the plan. `keep` is not a cleanup step: it is a
  copy-only export chosen before retirement. The other modes run only after
  the snapshot is verified and published, and only on the retired tree.
  `retention_days` applies only to an explicit `cpf quarantine purge`.

Additional decisions: name **cpforge**, command `cpf`; default
cleanup mode `trash`; float comparison methods configurable, integer answers
strict by default (`compare-spec.md`); custom checkers and interactive
problems with adaptive interactors ship in the initial feature release, with the two-process
mechanics (connected runs, relay) built in the CLI foundation's supervisor phase;
`distill` integrates through the generic `submit` stage rather than a built-in
adapter; search is unindexed, with bounds. No open questions are listed here.
