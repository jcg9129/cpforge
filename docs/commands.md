# Configured commands

```
cpf [--problem <ref>] cmd <name> [--profile <p>] [--json | --events[=output]] [-- <extra>...]
```

`cpf cmd <name>` runs `[commands.<name>]` from the user or workspace
configuration as a managed job, on Linux and macOS. On Windows it exits with
status 10 before creating a job.

```toml
[commands.hello]
argv = ["sh", "-c", "echo hi"]

[commands.bench]
needs_build = true
for_each_test = true
repeat = 3
argv = ["{artifact}"]
```

## Arguments

Arguments after `--` are appended unchanged to the configured `argv`, or to the
shell `args` of a shell command. They are never expanded: `{sources}` passed
after `--` reaches the program as the literal text `{sources}`.

The command runs after the workspace lock is released, so it may itself call
`cpf` (for example `cpf switch`).

## Context

A command needs a selected problem only when its fields require one: a problem
placeholder such as `{problem_id}` or `{problem_dir}`, `cwd.selector =
"problem"`, `needs_build` or `for_each_test`. Otherwise it needs no selection
and no language, and runs from anywhere inside the workspace. `--problem`
selects a problem for one invocation. `--profile` applies to the target
settings used by `needs_build` and `{entry}`.

`cpf cmd` must be run inside a workspace: job directories live under the
workspace's `.cpforge/runs/`. Outside a workspace it exits with status 8 and
creates nothing.

## Builds, tests and repetitions

- `needs_build = true`: the selected target is built once, before any run.
  `{sources}`, `{source}`, `{artifact}` and `{artifact_dir}` refer to that job's
  captured sources and build outputs. If the build fails, no iteration runs
  and the status is 3.
- `for_each_test = true`: one run per enabled test, in catalog order. The
  test's input and answer are frozen when the job starts; later edits do not
  affect the job. Each run reads the frozen input on stdin unless
  `stdin = "null"`. `{test_input}` and `{test_answer}` name the frozen files;
  using `{test_answer}` for a test without an answer is a plan error.
  With no enabled tests the command is refused with status 7.
- `repeat = N` (1 to 1000): each test (or the single command) runs N times,
  all iterations of one test before the next test. `{iteration}` is the
  1-based iteration number. Every iteration gets a fresh working directory.
- At most 10 000 runs may be planned, counting the build; a larger plan is
  refused with status 7 before anything runs.
- The first failed iteration (non-zero exit, signal, deadline, output limit, or
  a run that could not start) skips all later iterations as `stop_policy`,
  unless `keep_going = true`.
- Ctrl-C stops before the next iteration; the remaining iterations are
  reported as `cancelled`, and on Unix `cpf` then exits by the same signal.

When a run leaves more than `retention.cwd_bytes` in its own working
directory, the directory is cleared and a `CwdOverflow` warning is reported.
This never turns a successful run into a failure. Working directories outside
the job (such as `cwd.selector = "problem"`) are never measured or cleared.

## Output and exit status

`--json` prints the job report; `--events` streams job events (`--events=output`
also streams program output). The report lists each iteration's coverage and
one aggregate per test (one aggregate with `test: null` without
`for_each_test`): the number of iterations, how many succeeded, and the
minimum, median and maximum wall time of the successful iterations. For an even
count the median is the lower of the two middle values. Commands have no
answer comparison.

| Status | Meaning |
|---|---|
| 0 | every run succeeded |
| 1 | a run failed |
| 3 | the build failed (`needs_build`) |
| 7 | configuration or plan error, including a required problem that is not selected |
| 8 | workspace error: outside a workspace, an invalid `--problem` or saved selection, storage failures |
| 10 | a program could not be run or observed reliably |
| 11 | `--json` or `--events` output could not be written |

Cancellation exits by the signal on Unix (status 6 otherwise).

# Interrupting jobs

This applies to `cpf build`, `cpf test`, `cpf run`, `cpf cmd`, `cpf stress`,
`cpf interactor-test`, `cpf submit-prep` in stage mode and the `on_switch`
hooks of `cpf switch`, on Linux and macOS.

- **First Ctrl-C (`SIGINT`) or `SIGTERM`**: the job is cancelled. Running
  programs, including `after_test` and `on_switch` hooks, are stopped with
  their process groups, and runs not yet started are reported as `cancelled`.
  The report is still saved, `--json` still prints it, and the summary is still
  shown on stderr. `cpf` then exits by the same signal (a shell shows 130 or
  143). If the output cannot be written, the status is 11 instead.
- **A second signal at least 100 ms after the first** forces exit: `cpf` kills
  every running program's process group and exits at once by that signal,
  without waiting for the report. Records already saved remain.
- **During planning** (reading configuration and selecting the problem), a
  signal refuses the job before it is created: nothing is written under
  `.cpforge/runs/` and `--json` prints a refusal of class `cancelled`.
- **After the report has been delivered**, a single signal no longer changes
  the result or the status; a second one still forces exit.
- `cpf switch` itself, and commands that only read (`current`, `status`,
  `config`, and similar), keep the default signal behaviour: they stop
  immediately. A selection that `cpf switch` already committed stays committed
  when its hooks are interrupted.
- `cpf fetch`, `cpf search` and `cpf ls --archived` also listen for the
  first signal: they stop waiting (for the network, or for a slow
  filesystem), print what they have, and exit by the same signal.
  Source-copy `cpf submit-prep` does the same while it
  copies to the clipboard. A second signal forces exit.
- On Windows, Ctrl-C, Ctrl-Break and closing the console cancel the
  commands above that run there; the status is then 6.
- Programs that leave their process group (for example with `setsid`) are not
  stopped.

# Fetching problems

```
cpf fetch <url> [--only <label>]... [--all-in-section] [--disable-removed] [--offline] [--json]
cpf fetch <problem> | contest:<name> [--only <label>]... [--disable-removed] [--offline] [--json]
cpf fetch --history [<problem>]
```

`cpf fetch` creates or updates problems from AtCoder and CSES: their
metadata (title, limits, kind, URL) and their sample tests. It sends no
login, no cookies and only HTTPS `GET` requests, and runs no program, so it
works the same on Linux, macOS and Windows. The rules are in the sites
contract (requests, permit, cache, adapters) and storage contract §5.8
(what is written).

**What is fetched.** `<url>` is resolved without a request (sites contract
§8.4, §8.5):

- an AtCoder task URL (`https://atcoder.jp/contests/<c>/tasks/<t>`) or a
  CSES task URL (`https://cses.fi/problemset/task/<n>`): that problem,
  stored as `problems/atcoder-<t>` (`atcoder:<t>`) or `problems/cses-<n>`
  (`cses:<n>`);
- an AtCoder contest URL (`/contests/<c>` or `/contests/<c>/tasks`): every
  task of the contest, then the contest record
  `contests/atcoder-<c>/contest.toml`, which lists the members that are
  committed (storage contract §5.8.13);
- a CSES task URL with `--all-in-section`: every task of its problemset
  section, without a contest record.

A problem reference (`atcoder:abc300_a`, `cses-1068`, `.`, a
`<contest>/<label>` form) or `contest:<name>` fetches again from the URL
recorded in that problem's or contest's record. A local problem, or one
without a recorded URL, cannot be fetched (8).

A contest or section costs one listing request, then one request per
member. `--only <label>` (repeatable) keeps only those members: contest
labels, or task numbers for a CSES section. At most 50 members may be
selected; a larger selection or a label that is not in the listing is
refused after the listing and before any member request, with nothing
written (2).

**Updates never lose your edits.** Each problem is one transaction
(storage contract §5.8.4). Test files are never modified or deleted: a
changed sample is published as new files (`sample-1.r2.in`, …) and the
catalog is updated to name them. For each sample (storage contract
§5.8.5):

- new upstream: added and registered;
- changed upstream, not edited by you: the entry moves to the new files;
  the old files stay, shown as superseded by `cpf status`;
- edited by you: kept as it is; if upstream changed too, the upstream
  version is stored next to it as an unregistered pair, which
  `cpf test register` can adopt, and a warning names both the sample and
  the upstream copy (status 0);
- removed upstream: reported; `--disable-removed` disables its entry;
- removed by you from `tests.toml`: not added back.

When the upstream samples cannot be matched to yours without guessing
(renumbered, duplicated), or a record or test file changed since the fetch
began, nothing is registered and no record of that problem is changed: the
conflict is reported (status 9). A change detected only while the catalog
is being committed can leave the sample files already published as
unregistered downloads, listed in the fetch history (storage contract
§5.8.4 step 5, §5.8.8); your files and catalog are never changed by it.
Metadata is applied field by field: a field you edited that also changed
upstream is kept, and reported (status 9). Zero samples on a page that
was recognized are a warning, not a failure; your tests stay.

**Network.** Every request to a host goes through one per-user permit
shared by all cpf processes: at most one request at a time, 2 s apart for
AtCoder and 1 s for CSES, `Retry-After` and cooldowns persisted before the
next request (sites contract §4, §5). One invocation sends at most 200
requests within 15 minutes (§3.4). The permit lives in the user cache
directory (`CPF_CACHE_DIR`, else `http.cache_dir` in the user
configuration, else the OS cache directory); a network filesystem there is
refused (§4.4). Pages are cached in `.cpforge/cache/http/` and revalidated
on every online fetch. `--offline` (or `http.offline = true`) sends no
request and reads only the cache; it may create problems (§6).

**Output.** On stderr, one line per problem with its outcome
(`committed`, `unchanged`, `conflict`, `refused`, `failed`) and findings,
then warnings (such as `no_samples`, `language_fallback`,
`tolerance_hint`: hints are never applied). `--json` writes the `fetch`
document (sites contract §11.2, report contract §6.1); a failure before
the first problem is fetched (an unsupported URL, a failed contest
listing, a refused selection) writes the `error` document
(`command: "fetch"`) instead.
`cpf status` shows each problem's latest fetch attempt, superseded and
unregistered upstream files, and `fetch_incomplete` while an interrupted
fetch is pending (storage contract §5.8.12).

`cpf fetch --history` lists every fetch record of the problem (storage
contract §5.8.10) and its latest attempt. It only reads: no lock, no
network. It has no `--json` in this version.

**Interrupting.** Ctrl-C ends a wait for the permit or a transfer without
sending another request, and members not started are not fetched; a
problem already committing its files finishes first. An interrupted
fetch is completed or rolled back by the next `cpf fetch` of that problem
or by `cpf fsck --apply` (storage contract §5.8.7).

The global `--problem` flag is rejected (2): the problem is an operand.

| Status | Meaning |
|---|---|
| 0 | every selected problem was committed or unchanged (kept edits, including an edited sample whose upstream changed, removed samples and warnings are reported, not failures) |
| 2 | invalid arguments; `--only` naming a label that is not in the listing, or selecting more than 50 members; `--only` or `--all-in-section` with a selection they do not apply to; `--history` with `--json` |
| 7 | configuration error (configuration contract §3), including the `sites.*` and `http.*` keys |
| 8 | outside a workspace, lock busy, a local or unknown problem to fetch again; a problem refused (invalid records, an identity mismatch, an interrupted fetch that cannot be decided) or failed while writing (storage contract §5.8.4, §5.8.8) |
| 9 | a conflict was kept (samples that cannot be matched, a file changed during the fetch, an edited metadata field); a problem committed with its metadata, history or journal step pending or not applied; in a contest or section, some members committed or unchanged and others failed; the contest record not updated after its members were committed |
| 11 | `--json` output could not be written |
| 12 | a site or network failure (sites contract §8.3: `auth_required`, `challenge`, `rate_limited`, `not_found`, `not_available`, `unsupported`, `unrecognized`, `parser_failure`, `network`, `offline_miss`) of the URL, the listing or every problem that failed |

Cancellation exits by the signal on Unix (status 6 otherwise). When several
rows apply, for one problem or across a contest, the order is 2, 11,
cancellation, 7, 8, 9, 12 (report contract §5.2): one refused member gives
8, and a contest in which some members failed at the site while others
were fetched gives 9.

```sh
cpf fetch https://atcoder.jp/contests/abc300/tasks/abc300_a
cpf fetch https://atcoder.jp/contests/abc300 --only A --only B
cpf fetch . --disable-removed        # fetch the selected problem again
cpf fetch --offline cses:1068
```

# Workspace status

```
cpf [--problem <ref>] status [--json]
cpf status <problem> | <contest> | contest:<contest> | --all [--json]
cpf status [<problem> | <contest> | contest:<contest> | --all] <filters> [--json]
```

`cpf status` shows, for each selected problem:

- its tags and workflow status (the value set by `cpf mark`, shown as
  `workflow: solved`) from `problem.toml`, kept apart from everything below;
- registered files that are missing (sources and support files of the
  solution, files named by `tests.toml`);
- untracked files: files no record names (not registered, not listed in
  `[files] created` or `[files] user`) and not hidden by `[workspace] ignore`;
- how the current files compare with the last completed `cpf test` run of the
  `solution` target, with that run's verdict and finish time;
- for a fetched problem, its latest fetch attempt, superseded and
  unregistered upstream sample files, and `fetch_incomplete` while an
  interrupted fetch is pending (see "Fetching problems");
- the current submission and whether it is `stale` (see "Submission");
- open archive transactions (`archive_incomplete`, see "Archiving").

The comparison recomputes the source and test fingerprints from the current
files and configuration and compares them with the stored run fact:
`unchanged`, `source changed`, `tests changed`, `source and tests changed`, or
`unknown` (no fact, an unreadable, malformed or newer-format fact, or inputs
that cannot be fingerprinted; a note says which). `unchanged` means the
recorded inputs and build settings are equal, not that the program behaves the
same; when the run's build was a shell stage, whose script cpf does not
inspect, it is shown as `unchanged (capture unchecked: shell stage)`. A
`cpf test --profile` run is compared with the default profile's settings, so
it shows as `source changed`.

Without an argument the problem is selected as for every other command
(`--problem`, the current directory, then `cpf switch`). A name selects that
problem; if no problem has that name, the contest of that name, whose members
are shown in `contest.toml` order. `contest:<name>` always means the contest.
`--all` shows every problem in `problems/`, by name.

Filters (see "Filters" below) keep only the matching problems of that
selection; filters without a selection mean `--all`. Without filters the
command behaves as before.

Status only reads: it takes no lock, creates no file and never deletes
anything. `--json` prints one `status` document (report contract §6.1).

| Status | Meaning |
|---|---|
| 0 | status shown (findings such as missing files do not change it) |
| 2 | a filter value is not valid |
| 8 | outside a workspace, an unknown problem or contest, unreadable records |
| 11 | `--json` output could not be written |

# Tags and workflow status

```
cpf tag <target> <tag>...
cpf untag <target> <tag>...
cpf mark <target> <status>
```

`<target>` is a problem (`two-sum`, `local:two-sum`, `cses:1068`), `.` for
the problem the current directory or `cpf switch` selects, or, for `tag`
and `untag` only, `contest:<name>`. A name never falls back to a contest
here; use `contest:<name>`.

Tags are free-form words of 1–32 ASCII letters, digits, `-` and `_`,
starting with a letter or digit. They are compared ignoring ASCII case:
`DP` and `dp` are the same tag, and the first spelling is kept. `cpf tag`
adds tags that are not there yet; `cpf untag` removes them. Problem tags
live in `[problem] tags` of `problem.toml`, contest tags at the top level of
`contest.toml`; they are separate, so filtering problems by a tag never
matches a contest's tag. An `upsolve` tag can sit next to any status.

The **workflow status** is yours: `new`, `in-progress`, `solved`,
`skipped`, plus the names in `[status] values` of your user or workspace
configuration. A problem never marked counts as `new`. Passing tests never
changes it. The first `cpf mark p solved` records `solved_at`; later marks,
including solving again after reopening, keep that first time. Every change records `updated_at`; new
problems get `created_at`. Times are stored in UTC, as
`2026-11-24T10:00:00.000Z`.
Contests have no workflow status of their own; `cpf ls --contests` counts
their members per status.

The commands edit only these keys; comments, formatting and every other key
stay as they are. cpf rereads the file just before replacing it and, if an
editor changed it, starts over once from the new contents, then refuses. A
save that lands in the short moment between that last check and the
replacement is overwritten, as with every cpf record edit; an editor that
later saves an older buffer overwrites cpf's change. A key you wrote by hand that cpf
cannot read (`tags = "dp"`, an invalid date) is reported by `cpf status`,
`cpf ls` and `cpf fsck`, never rewritten: the command that would change it
refuses until you fix it. Such a key never stops builds or tests.

| Status | Meaning |
|---|---|
| 0 | done, or nothing to change |
| 2 | an invalid tag or status name, no tag, `--problem`, `mark` on a contest |
| 7 | `mark`: a status that is neither built-in nor in `[status] values` |
| 8 | outside a workspace, unknown target, unreadable record, a hand-written key cpf cannot read, lock busy, concurrent edits |

# Notes

```
cpf [--problem <ref>] notes [<problem>] [--path] [--json]
```

`cpf notes` creates an empty `notes.md` in the problem directory if there
is none and prints its absolute path, so `$EDITOR "$(cpf notes)"` opens it.
An existing `notes.md` is never changed. `--path` only prints the path and
creates nothing. Notes are plain text that cpf only reads (for
`cpf search`); they are never listed as untracked. `--json` prints the
`notes` document (report contract §6.1) instead of the bare path. A
problem operand together with `--problem`, or a contest target, is a usage
error (2). Exit 0; 8 when the problem cannot be resolved or, without
`--path`, `notes.md` exists but is not a regular file (`--path` only reports
that and exits 0; storage contract §5.9.5); 11 when `--json` output could
not be written.

# Listing and searching

```
cpf ls [<filters>] [--json]
cpf ls --contests [--tag <t>]... [--site <s>]... [--json]
cpf ls --archived [<filters>] [--root <dir>]... [--json]
cpf ls --archived --contests [--root <dir>]... [--json]
cpf search <term>... [<filters>] [--no-notes] [--no-archives] [--root <dir>]... [--json]
```

`cpf ls` lists every problem with its workflow status, tags, title, dates
and contests, by name, then every entry of `problems/` that is not a usable
problem, with the reason. `cpf ls --contests` lists contests with their
tags and the number of members per workflow status.

`cpf search` finds problems in which every term occurs, ignoring case and
accents written as combining marks, in the identity, title, URL, tags,
workflow status or the first 64 KiB of `notes.md` (`--no-notes` skips the
notes). It reads files directly, without an index. One search reads at most
100 000 directory entries and files and 64 MiB; if a bound is reached,
notes are cut at 64 KiB, a file cannot be read or the search is
interrupted, the result says it is **incomplete** and why. Unreadable or
broken problems are always reported, never hidden behind "no matches".

**Archived problems.** `cpf ls --archived` lists the snapshots in the
archive roots instead of the workspace: the roots of `archive.roots`, in
order, then each `--root`, with root, path, snapshot id, completeness and
the problem's summary fields (archive contract §8). `--archived
--contests` lists contest snapshots. `cpf search` also searches every
snapshot's summary and the first 64 KiB of its notes after the workspace,
within the same bounds; `--no-archives` searches the workspace only.
Results say whether they are `active` or a `snapshot`. A root that is
missing or not an archive root is reported, never skipped silently; a
configuration error only makes `archive.roots` unavailable (reported):
these commands never exit 7. There is no timeout for a slow network root:
Ctrl-C prints what was found so far (see "Interrupting jobs").

**Filters** (same for `ls`, `status` and `search`):

| Flag | Keeps problems |
|---|---|
| `--status <s>` | with that workflow status (repeat for any of several) |
| `--tag <t>` | with that tag (repeat: all of them) |
| `--site <s>` | from that site (`local`, `atcoder`, `cses`; repeat for any) |
| `--contest <c>` | listed in that contest (repeat for any) |
| `--created`, `--updated`, `--solved <range>` | with that date in the range |
| `--archived-in <range>` | archived in the range (active problems restored from a snapshot, and snapshots with `--archived` or in `search`) |

A range is a day `2026-11-24`, `2026-11-01..2026-11-24` (inclusive),
`2026-11-01..` or `..2026-11-24`, in your local time zone (`TZ`, else the
system setting); times are stored in UTC and converted, so a problem
solved at 00:30 local time counts for that local day, also across DST
changes. Different flags must all match.

These commands only read: no lock, no file created. They start no program,
so they work the same on Linux, macOS and Windows, as do `tag`, `untag`,
`mark` and `notes`.

| Status | Meaning |
|---|---|
| 0 | listed or searched (also when incomplete or with findings) |
| 2 | invalid flags, filter values or search terms; a problem-only filter with `--contests` |
| 8 | outside a workspace, `problems/` or `contests/` unreadable, an unknown `--contest` |
| 11 | `--json` output could not be written |

`cpf search` and `cpf ls --archived` exit by the signal when interrupted on
Unix (status 6 otherwise).

# Stress testing

```
cpf [--problem <ref>] stress [--profile <p>] [--deadline <d>] [--seed <u64>]
                             [--iterations <n>] [--time <duration>] [--keep-going]
                             [--json | --events[=output]]
cpf [--problem <ref>] stress --replay <id> [--profile <p>] [--deadline <d>] [--json | --events[=output]]
cpf [--problem <ref>] stress --replay-binaries <id> [--json | --events[=output]]
cpf [--problem <ref>] stress ls [--json]
cpf [--problem <ref>] test add --from-stress <id>
```

`cpf stress` runs the selected problem's solution against generated inputs
until a budget is used up or an iteration fails. The generator, optional
validator, brute-force reference and judging come from `[stress]` in
`problem.toml` (configuration contract §9.2); a batch or interactive
problem is required. Each iteration `k` uses seed `start + k`: `--seed`
sets the start, otherwise a random start is printed and recorded, so the
run can be repeated (stress contract §3.2). The budgets are
`--iterations` (default `[stress] iterations`, 10 000) and `--time`
(default `[stress] time_s`, 60 s); the time budget only stops new
iterations. Without `--keep-going` the first failing iteration stops the
job; an infrastructure failure always does (§5). The job is a managed job
like `cpf test`: `--json` prints its report, `--events` streams iteration
events (report contract §§6, 7), and its working files are released after
every iteration, so any number of iterations runs in bounded space (§6).

A failing iteration is saved under `problems/<p>/stress/failures/<id>/`
with its input, outputs, seeds and, when available, the brute answer
(stress contract §7; at most 20 per job). Its id is
`f-<epoch>-<job>-<iteration>` (§7.1). `cpf stress --replay <id>`
rebuilds the current sources and runs that iteration again with its saved
input and seeds; `--replay-binaries <id>` runs the saved programs instead,
when they were preserved and on the same OS and architecture (§8). A
replay says whether the failure reproduced (same class).

`cpf stress ls` lists the saved failures with their class, reason,
iteration and seed; `--json` writes the `stress_failures` document
(report contract §4.15). It never verifies hashes: they are checked when a
failure is used. `cpf test add --from-stress <id>` registers a saved
failure's input, and its answer when the brute answer was validated, as
a test `stress-<n>` (§2.4).

`--seed`, `--iterations`, `--time` or `--keep-going` with `--replay` or
`--replay-binaries`, both replay options together, `--iterations 0` and an
id outside the id grammar are usage errors (2). A seed range that would
pass `u64::MAX` is refused before anything runs (7). Saved failures are
your work: `cpf clean` never removes them and `cpf gitignore` offers them
only as a comment.

On Windows, `cpf stress` in every mode is refused with status 10 before
anything is created (7 first for a configuration error);
`cpf stress ls` and `cpf test add --from-stress` work on every OS.

| Status | `cpf stress` |
|---|---|
| 0 | every iteration passed (a replay: its iteration passed) |
| 1 | the solution failed an iteration (`candidate_failure`), also in a replay |
| 3 | a build failed (solution, generator, validator, brute or judge) |
| 4 | no iteration ran (`no_iterations`) |
| 5 | a generator, validator, brute or judge failed, so no verdict on the solution |
| 7 | configuration or plan error, an unsupported problem kind; a replay of a failure saved for another problem shape, or `--replay-binaries` without the needed programs or on another OS or architecture |
| 8 | workspace error, an unknown or unusable saved failure |
| 10 | a program could not be run or observed reliably, an iteration's files could not be released (`stress_cleanup_failed`), Windows |
| 11 | `--json` or `--events` output could not be written |

`cpf stress ls`: 0 (any entries, any state), 8, 11. `cpf test add
--from-stress`: 0, 8 (the failure is not valid or its files differ from
its record; nothing published), 9. Cancellation exits by the signal on Unix
(status 6 otherwise).

```sh
cpf stress --iterations 100000 --time 5m
cpf stress --seed 4242 --keep-going --json > stress.json
cpf stress ls
cpf stress --replay f-0123456789abcdef0123456789abcdef-12-417
cpf test add --from-stress f-0123456789abcdef0123456789abcdef-12-417
```

# Interactor test

```
cpf [--problem <ref>] interactor-test [--case <id>]... [--profile <p>] [--deadline <d>]
```

`cpf interactor-test` checks an interactive problem's interactor against
the known solutions declared in `[check.interactor_test]` of
`problem.toml`: `good` solutions it must accept and `bad` solutions it must
reject (configuration contract §9.1.7). Passing it does not prove that an
adaptive interactor is correct; it only catches interactors that accept a
known wrong solution or reject a known right one.

Each declared solution, the `good` ones in order and then the `bad` ones,
is judged in its own job exactly as `cpf test` judges the solution: the
same tests (`--case`, else every enabled test), the same `[check]`
settings, interactor, seeds and display profile. `--profile` and
`--deadline` apply as for `cpf test`. Each job has its own directory under
`.cpforge/runs/` and its `report.json`, with the solution named `good-<i>`
or `bad-<i>` (from 1). These jobs publish no run fact and run no
`after_test` hook: the declared solutions are not your solution.

- A `good` solution meets its expectation when its job passes (or scores
  the full scale).
- A `bad` solution meets its expectation when at least one test gets a
  solution verdict (wrong answer, presentation error, time limit, idle,
  session limit, output limit, runtime error) or a score below the full
  scale.

A job that cannot judge (a build failure, a checker or interactor failure,
a configuration, workspace or infrastructure error, cancellation) stops the
command: later solutions are listed as not run. On stderr each solution
gets one line: role and index, job id, verdict and whether the expectation
was met; then a summary. There is no `--json` or `--events` in this version
(2); the per-job reports carry the details.

The command requires the problem kind `interactive` with
`[check.interactor_test]`, and at least one declared solution (7
otherwise). On Windows it is refused with status 10 before any job is
created (7 first for a configuration error).

| Status | Meaning |
|---|---|
| 0 | every expectation was met |
| 1 | a `good` solution failed, or a `bad` solution passed |
| 3 | a build failed (a declared solution, the interactor or the checker) |
| 4 | an expectation could not be decided: a `good` solution not fully checked (no answer, no tests, an incomplete score), or a `bad` solution that neither passed nor failed or scored below the full scale on any test |
| 5 | the interactor, the checker or a validator failed |
| 7 | configuration or plan error, wrong problem kind, no declared solution |
| 8 | workspace error |
| 10 | a program could not be run or observed reliably; Windows |

When several apply, over all jobs, the order is that of report contract
§5.2: 7, 8, 10, 3 and 5 come before 1, then 4, then 0. Cancellation exits
by the signal on Unix (status 6 otherwise).

# Submission

```
cpf [--problem <ref>] submit-prep [--source-copy] [--no-test] [--profile <p>]
                                  [--judge <name>] [--copy | --no-copy]
                                  [--json | --events]
```

`cpf submit-prep` prepares what you submit to the judge and keeps it as an
immutable version under `.cpforge/submit/<p>/`, with a `current`
reference to the latest one (submission contract §8). It never submits
anything.

- **Source copy** (when the language has no `submit` stage, or with
  `--source-copy`): the solution's files (`solution_files`, else the
  sources and support files of the solution target) are copied as they
  are, without building or testing (`untested`). An explicitly empty
  `solution_files` is refused: nothing to submit (7).
- **Stage mode** (the language has a `[languages.<l>.submit]` stage,
  submission contract §3.2): a managed job
  runs the declared tool (for example a bundler), optionally checks its
  version first and selects the artifact (§6). Unless `--no-test` or
  `test = false`, it then builds and tests the artifact like `cpf test`, in
  a fresh directory **outside the workspace** holding only the artifact and
  the provided files, with the accident guard's operand check (§6.5–§6.6,
  §7). That isolation is narrower than it may sound: an absolute include
  or file path into the workspace, an include path inherited from the
  environment, or a wrapper program is not detected (§1.2). With
  `--no-test` or `test = false` the artifact is neither built nor tested
  and the version is `untested`. A version is published only when the job
  passed (or scored the full scale), or, without the artifact test, once
  an artifact was selected (§6.7). `--judge` sets `{judge}` for the stage; `--profile` selects the
  build profile of the artifact test.

The new version becomes `current` only after it is complete. On stderr:
`submission <id> (<mode>, tested|untested)`, the path of the file to
submit and, when known, the judge's submit page. `--copy` (or
`submit.clipboard = true`, overridden by `--no-copy`) copies the primary
file to the clipboard: `pbcopy` on macOS, `wl-copy` or `xclip` on Linux,
the Win32 clipboard on Windows with no process; a clipboard failure is a
warning and never changes the status (§9.3). `--json` prints the `submit`
document in source copy and the job report in stage mode (report contract
§6.1); `--events` is for stage mode only. `cpf status` says when the
current submission is `stale` (sources changed since); `cpf clean`
removes the versions that are not current.

`--profile`, `--judge` or `--events` with source copy are usage errors
(2). Answer, multi-file and other unsupported problem kinds are refused
(7). On Windows, source copy works; stage mode is refused with status 10
before anything is created (7 first for a configuration error).

| Status | Source copy | Stage mode |
|---|---|---|
| 0 | the version was published and is `current` | the same (the artifact test passed, or `--no-test`) |
| 1 | — | the artifact failed a test |
| 3 | — | the submit tool failed (`submit_failed`), or a build of the artifact test failed |
| 4 | — | the artifact test did not check every test (no answer, no tests) |
| 5 | — | a checker, validator or interactor failed |
| 7 | configuration error, nothing to submit, unsupported kind | the same, and a declared tool version that does not match (`submit_version_mismatch`) |
| 8 | workspace error, lock busy, the version could not be published | the same |
| 9 | the version was published but `current` was not updated | the same |
| 10 | — | a program could not be run or observed reliably; Windows |
| 11 | `--json` output could not be written | `--json` or `--events` output could not be written |

Cancellation exits by the signal on Unix (status 6 otherwise). A version
already published stays published.

# Archiving

```
cpf archive <target> [--to <dir>] [--keep-profile <name>] [--cleanup <mode>]
                     [--on-existing <mode>] [--apply [--plan <hash>]] [--json]
cpf archive --resume [--apply] [--json]
cpf archive --close <txn> [--apply]
cpf quarantine ls
cpf quarantine purge [<id>...] [--apply]
cpf restore <snapshot> [--into <dir>] [--partial] [--root <dir>]... [--json]
```

`cpf archive` moves a problem you are done with out of the workspace: it
writes a verified **snapshot** to an archive root, then retires the problem
from `problems/` (archive contract). `<target>` is a problem reference
(`.`, a name, an identity, `<contest>/<label>`) or `contest:<name>`. None
of these commands starts a program, runs a hook or uses the network; they
run the same on Linux, macOS and Windows.

**Plan, then apply.** Without `--apply`, `cpf archive` prints the plan and
changes nothing: per class
(`sources`, `tests:sample`, `tests:custom`, `tests:stress`, `stress`,
`notes`, `metadata`, `fetch`, `submit`, `other`; archive contract §3.3) the
files and bytes kept or discarded, the destination, whether the problem
will be retired and why not, what cleanup will do, and the plan hash
(archive contract §3.7). `--json` writes it as the `archive_plan`
document. `--apply` computes a fresh plan and carries it out;
`--apply --plan <hash>` refuses (8) when the fresh plan differs from the
one you reviewed.

**Configuration** (`[archive]`, configuration contract §7.6): `roots`
(searched by `ls --archived`, `search` and `restore`), `default_root`,
`layout`, `keep`/`discard` classes, `cleanup` and `on_existing`, and named
profiles selected with `--keep-profile`. `--to <dir>` replaces the root for one run; the plan warns when
it is not in `roots`. No root from any source is a configuration error
(7).

**What happens to the files.** The snapshot is complete and verified
before anything in the workspace changes. Then the problem directory is
renamed into `.cpforge/archive/<txn>/` and cleaned up by `--cleanup`
(archive contract §6):

- `trash` (default): to the OS trash (freedesktop trash, the macOS Trash
  through the system API, the Windows Recycle Bin); when the trash cannot
  be used safely, the files go to the quarantine instead;
- `quarantine`: to `.cpforge/quarantine/<txn>/`, kept until you purge it;
- `delete`: removed after a last check; the plan warns that bytes written
  later through a file already open are lost;
- `keep`: snapshot only (an export); the workspace is not changed.

A file that changed since the snapshot, or appeared after the plan, goes
to the quarantine in every mode, and under `delete` so do the files the plan
discards (archive contract §6.2). The problem stays in place (`snapshot_only`, status 9) when a
contest other than the one being archived lists it, or when something
changed between the plan and retirement. Archiving refuses while any job
of the workspace is running or a fetch of the problem is pending (8). An
existing snapshot at the destination is an error by default;
`--on-existing new` makes another, `reuse-identical` reuses an equivalent
one (§5.7).

`cpf archive contest:<name>` archives the contest and every member, each
with its own snapshot, then a contest snapshot that is complete only when
every member is (§5.6). An interrupted contest archive is resumed by
running it again or by `--resume --apply`.

**Finishing interrupted work.** `cpf archive --resume` shows the archive
transactions whose cleanup is not finished; `--apply` continues them
(every `cpf archive --apply` also does this first). `cpf archive --close
<txn> --apply` ends a transaction whose cleanup cannot finish (for
example a file the trash refuses) by moving what is left to the
quarantine; nothing is deleted (§5.5). `cpf status` and `cpf fsck` report
open transactions.

**Quarantine.** `cpf quarantine ls` lists the quarantine entries with
their state, age, origin and size; it only reads. `cpf quarantine purge`
plans the removal of the named entries, or without ids every ready entry
at least `archive.quarantine.retention_days` old (default 30);
`--apply` deletes them. Nothing is ever purged automatically; an entry
of an unfinished transaction is never purged (8 when named).

**Restore.** `cpf restore <snapshot>` takes a snapshot id (looked up in
`archive.roots` and each `--root`; the same id in two roots is refused
unless `--root` narrows it) or a path to a snapshot directory, verifies
its manifest and every file's hash, and publishes the problem as
`problems/<dir>` with its fetch history, without overwriting anything
(archive contract §7). It needs no `--apply` because it never replaces or
removes anything. `--into <dir>` restores a copy as the local problem
`local:<dir>`, keeping the original identity as `origin` (its fetch
history is not restored). A contest snapshot restores its members and then
the contest; an incomplete one needs `--partial` (`--into` with a contest
→ 2). Restoring a problem that is already restored changes nothing and
exits 0. Content that the keep profile discarded, run facts and job
directories are not restored; external programs and paths the snapshot
refers to are listed, with whether they exist now. `--json` writes the
`restore` document.

The global `--problem` flag is rejected by these commands (2).

| Status | Meaning |
|---|---|
| 0 | done: the plan shown; the problem retired with cleanup complete, exported, or reused; every transaction touched by `--resume` or `--close` complete; restored or already restored; purged or nothing to purge |
| 2 | invalid arguments: `--plan` without `--apply`; `--resume` or `--close` with a target or with `--to`, `--keep-profile`, `--cleanup`, `--on-existing`; `--json` with `--close` or `quarantine`; `--into` with a contest snapshot |
| 7 | configuration error: no archive root, an invalid `[archive]` table, an unknown `--keep-profile`; for `restore` by id, configuration that cannot be read |
| 8 | refused with nothing written: a running job, a pending fetch, an unfinished transaction of the problem, invalid records, an existing snapshot, a changed plan, an unknown, ambiguous or invalid snapshot, an incomplete contest snapshot without `--partial`, a destination that exists, a pending quarantine entry named; lock or enumeration failure |
| 9 | the snapshot is committed but the problem was not retired (`snapshot_only`) or its cleanup is incomplete; a transaction left incomplete by `--resume` or `--close`; a problem restored without its fetch history; a purge that left a selected entry |
| 11 | `--json` output could not be written |

Per form (report contract §5.5): `cpf archive <target>` without `--apply`
exits 0, 7, 8 or 11, with `--apply` also 9; `cpf archive --resume` (with or
without `--apply`) 0, 8, 9 or 11; `cpf archive --close` 0, 8 or 9 (archive
contract §5.4); `cpf restore` 0, 7, 8, 9 or 11; `cpf quarantine ls` 0 or 8;
`cpf quarantine purge` 0 or 8, and 9 with `--apply`.

```sh
cpf archive .                        # review the plan
cpf archive . --apply --plan 3f5c…   # apply exactly that plan
cpf archive contest:abc300 --cleanup quarantine --apply
cpf ls --archived --tag dp
cpf restore p-20261124T100000000Z-0123456789abcdef --into abc300a-again
cpf quarantine purge --apply
```

# Cleaning

```
cpf clean [--apply]
```

`cpf clean` lists what it would remove: every job directory under
`.cpforge/runs/` that no running cpf holds, leftovers of interrupted
removals, and `.cpforge/cache/` (which holds the page cache of `cpf fetch`;
the per-user request permit outside the workspace is never touched).
`cpf clean --apply` removes them. Jobs that are still running are kept,
including a running `cpf stress` or `cpf submit-prep`, as are job directories
written by a newer cpf and ones whose records cannot be read (each is
reported). Download areas left by an interrupted `cpf fetch` are removed by
the leftover cleanup that every locking command performs (storage contract
§5.8.3), `cpf clean` included.

It also lists and removes the submission versions that are no longer
current: in `.cpforge/submit/<problem>/`, every version that the
problem's `current` reference does not name (all of them when there is no
reference yet). When the reference cannot be read or names a missing
version, nothing of that problem is removed (reported). Removing a job
directory of `cpf submit-prep` also removes the temporary test directory
it created outside the workspace, if a crash left one behind.

Nothing outside `.cpforge/runs/`, `.cpforge/cache/` and those submission
versions is removed: the current submission of each problem, run facts,
fetch records (`.cpforge/fetch/`), archive transactions and the quarantine
(`.cpforge/archive/`, `.cpforge/quarantine/`; see "Archiving"), saved stress
failures, notes, the job counter, `state.toml`, the lock, and all problem and
contest files are kept. Like every command that takes the workspace lock, both forms first clean
up temporary files left by an interrupted cpf.

| Status | Meaning |
|---|---|
| 0 | done (or nothing to remove) |
| 8 | outside a workspace, or the workspace lock or `.cpforge/runs/` is unusable |
| 9 | `--apply`: something selected could not be removed (it is retried next time) |

`cpf clean` starts no program and works the same on Linux, macOS and
Windows (on Windows, a directory with a file still open inside cannot be
renamed: it is kept and retried).

# Git ignore patterns

```
cpf gitignore [--write]
```

`cpf gitignore` prints the patterns for content cpf owns:

```gitignore
.cpforge/runs/
.cpforge/cache/
.cpforge/staging/
.cpforge/downloads/
.cpforge/submit/
.cpforge/archive/
.cpforge/quarantine/
# optional: .cpforge/facts/
# optional: .cpforge/fetch/
# optional: problems/*/stress/failures/
```

Run facts are optional: commit them to share "last completed run" results.
Fetch records are optional too: they record which test files were
downloaded; without them, another checkout treats those files as your own.
Saved stress failures are your work and may be worth committing, but they
can be large and hold programs built for one platform.

`cpf gitignore --write` keeps these lines in one block of `<root>/.gitignore`,
between the lines `# >>> cpforge` and `# <<< cpforge`. It creates the file if
there is none, appends the block if the file has no markers, and otherwise
replaces only the lines between the markers; every other byte of the file is
kept. It refuses, changing nothing, when the markers are incomplete,
duplicated or reversed, when `.gitignore` is a link, not a regular file or has
several hard links, or when the file changes while cpf is updating it. It exits
0 on success and 8 when it refuses.

# Adopt and ignore files

```
cpf [--problem <ref>] adopt <path>
cpf ignore <pattern>
```

`cpf adopt notes.txt` adds an existing regular file to `[files] user` in the
selected problem's `problem.toml`. The path is relative to the **problem
directory**, even when invoked from the workspace root or a subdirectory.
It must use portable path components, with no `.` or `..`, absolute path,
link component or spelling collision. Files already registered as sources,
support files, solution files or tests are refused, as are files already
tracked as created by cpf. Repeating adoption of an adopted file succeeds
without rewriting the record. Adoption changes metadata only; file contents
stay intact. The record edit takes the workspace lock and preserves comments
and unknown fields, with a final check for concurrent editor changes.

`cpf ignore '*.swp'` validates the pattern and prints:

```toml
[workspace]
ignore = ["*.swp"]
```

Add the pattern to `[workspace] ignore` in `cpforge.toml`, or in your user
configuration. Merge it into an existing array rather than duplicating the
workspace table. The workspace array replaces the user configuration's array.
The command only prints a suggestion; repeated calls produce the same output
and never rewrite configuration or create a lock file.

Patterns match problem-relative paths with `/` separators. `*` matches
characters within one component, `?` matches one character, and `**` as an
entire component matches zero or more components. A pattern without `/`
matches the final component at any depth; a pattern with `/` starts at the
problem directory (`scratch/**`, for example). Empty patterns, leading or
trailing `/`, empty or dot components, partial `**` components, NUL, `!`,
`[`, `]` and backslashes are invalid. Matching is exact on Linux and ignores
ASCII case on Windows and macOS. Ignore hides untracked files from status;
it does not change provenance or permit deletion.

Both commands exit 0 on success and 8 on workspace, validation or storage
failure. Neither accepts `--json` or `--events`.

# Check workspace records

```
cpf fsck [--apply]
```

`cpf fsck` takes the workspace lock and reports dangling contest references,
problem directories without `problem.toml`, record schema/classification
problems, overlapping provenance, invalid `[files]` entries, orphan facts,
facts for unknown targets, unexpected entries in reserved trees and the
state of the admission sequence (`.cpforge/job-seq`). Invalid file provenance
is reported and ignored by status as well. Broken or newer-format problem
metadata does not establish that the problem was deleted.

With `--apply`, cpf can:

- remove references to absent problems from valid contest records, preserving
  the order, labels, comments and unknown fields of surviving entries;
- remove valid or malformed fact files belonging to absent problems, leaving
  newer-format and unreadable facts intact; facts for unknown targets of
  existing problems are only reported;
- replace a missing or malformed admission sequence with a fresh epoch and
  counter zero, after probing every directory in `runs/`, including
  interrupted reclamations and newer-format jobs. A held or unprobeable lease
  refuses sequence repair and lists the blocking jobs. Unreadable and
  newer-format sequences are preserved.

Only record repairs are performed. User files, run directories, cache
contents and `.cpforge/lock` are preserved; no process id or file age is used
to decide whether a lock is abandoned. Both modes perform the normal locked
operation's leftover recovery: abandoned staging contents and reserved
`.cpf-tmp-` files in metadata recovery locations are removed before inspection.
Empty orphan fact directories are left in place. Sequence repair is checked
before other repairs, so a blocking lease prevents that invocation's repairs.
Individual record repairs are atomic; the whole repair set is not one transaction.

Findings, including findings that cannot be repaired, exit 0. Lock refusal,
a failed repair or an operational storage failure exits 8. This command has
human-readable output only; `--json` and `--events` are rejected.

# Exit statuses

Every `cpf` command exits with one of these statuses (report contract
§5.1). Each code has one meaning on every command; the sections above say
which codes each command can return. When several conditions hold, the
status is that of the first row of report contract §5.2 that applies:
usage, a forced exit, an internal error, an output failure, cancellation,
then configuration, workspace, partial, site, infrastructure, build,
judging, solution failure, not verified, success.

| Status | Name | Meaning | Returned by |
|---|---|---|---|
| 0 | success | the command did what was asked | every command |
| 1 | failed | the solution or a program failed (wrong answer, presentation error, time limit, idle, session limit, output limit, runtime error, a score below the full scale); `interactor-test`: an expectation contradicted | `test`, `run`, `cmd`, `stress`, `interactor-test`, `submit-prep` (stage) |
| 2 | usage | invalid arguments or a rejected combination; nothing was started (except a refused `cpf fetch` contest or section selection, refused after its one listing request) | every command |
| 3 | build_failed | a build failed, or the submission tool failed | `build`, `test`, `run`, `cmd`, `stress`, `interactor-test`, `submit-prep` (stage) |
| 4 | not_verified | nothing failed, but not everything was checked (no answer, a judge's cleanup not verified, no tests, no iteration) | `test`, `stress`, `interactor-test`, `submit-prep` (stage) |
| 5 | checker_failed | judging could not decide: a checker, validator, interactor, generator or brute failed, or invalid test data | `test`, `stress`, `interactor-test`, `submit-prep` (stage) |
| 6 | cancelled | cancelled on Windows, or through the API; on Unix cpf exits by the signal instead (a shell shows 130 or 143) | job commands, `fetch`, `search`, `ls --archived`, `submit-prep` |
| 7 | config | configuration or plan error, an unsupported problem kind, a missing context, a workflow status outside the vocabulary, a submission tool of another version | most commands |
| 8 | workspace | outside a workspace, a selection error, an unreadable record, lock busy, a storage failure, a refusal with nothing written | every workspace command |
| 9 | partial | part of the work is committed and the rest is not, or the existing state was kept where an update conflicted; rerunning, or resolving what is reported, continues | `add`, `test add`, `clean --apply`, `fetch`, `archive`, `restore`, `quarantine purge --apply`, `submit-prep` |
| 10 | infrastructure | a program could not be run or observed reliably, or its resources released; on Windows, every command that would start a program | job commands, `stress`, `interactor-test`, `submit-prep` (stage) |
| 11 | delivery | the requested `--json` or `--events` output could not be written | commands with `--json` or `--events` |
| 12 | site | `cpf fetch`: the site refused or failed, the network failed, or an offline fetch found no cached page | `fetch` |
| 70 | internal | a bug in cpf, detected and reported | every command |

Codes 13–63 are reserved for later features. cpf never chooses 126, 127
or a code of 128 or more: on Unix those mean the command was killed by
the signal shown (128 + signal number).
