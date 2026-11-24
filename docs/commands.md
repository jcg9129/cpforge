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

This applies to `cpf build`, `cpf test`, `cpf run`, `cpf cmd` and the
`on_switch` hooks of `cpf switch`, on Linux and macOS.

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
- Programs that leave their process group (for example with `setsid`) are not
  stopped.

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
  `solution` target, with that run's verdict and finish time.

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
`cpf search`); they are never listed as untracked. Exit 0, or 8 when the
problem cannot be resolved or `notes.md` is not a regular file.

# Listing and searching

```
cpf ls [<filters>] [--json]
cpf ls --contests [--tag <t>]... [--site <s>]... [--json]
cpf search <term>... [<filters>] [--no-notes] [--json]
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

**Filters** (same for `ls`, `status` and `search`):

| Flag | Keeps problems |
|---|---|
| `--status <s>` | with that workflow status (repeat for any of several) |
| `--tag <t>` | with that tag (repeat: all of them) |
| `--site <s>` | from that site (`local`, `atcoder`, `cses`; repeat for any) |
| `--contest <c>` | listed in that contest (repeat for any) |
| `--created`, `--updated`, `--solved <range>` | with that date in the range |

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
| 2 | invalid flags, filter values or search terms |
| 8 | outside a workspace, `problems/` unreadable, an unknown `--contest` |
| 11 | `--json` output could not be written |

# Cleaning

```
cpf clean [--apply]
```

`cpf clean` lists what it would remove: every job directory under
`.cpforge/runs/` that no running cpf holds, leftovers of interrupted
removals, and `.cpforge/cache/`. `cpf clean --apply` removes them. Jobs that are
still running are kept, as are job directories written by a newer cpf and ones
whose records cannot be read (each is reported).

Nothing outside `.cpforge/runs/` and `.cpforge/cache/` is removed: run facts,
the job counter, `state.toml`, the lock, and all problem and contest files are
kept. Like every command that takes the workspace lock, both forms first clean
up temporary files left by an interrupted cpf.

| Status | Meaning |
|---|---|
| 0 | done (or nothing to remove) |
| 8 | outside a workspace, or the workspace lock or `.cpforge/runs/` is unusable |
| 9 | `--apply`: something selected could not be removed (it is retried next time) |

# Git ignore patterns

```
cpf gitignore [--write]
```

`cpf gitignore` prints the patterns for content cpf owns:

```gitignore
.cpforge/runs/
.cpforge/cache/
.cpforge/staging/
# optional: .cpforge/facts/
```

Run facts are optional: commit them to share "last completed run" results.

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
