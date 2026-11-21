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

# Workspace status

```
cpf [--problem <ref>] status [--json]
cpf status <problem> | <contest> | contest:<contest> | --all [--json]
```

`cpf status` shows, for each selected problem:

- its tags and `status` from `problem.toml`, kept apart from everything below;
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

Status only reads: it takes no lock, creates no file and never deletes
anything. `--json` prints one `status` document (report contract §6.1).

| Status | Meaning |
|---|---|
| 0 | status shown (findings such as missing files do not change it) |
| 8 | outside a workspace, an unknown problem or contest, unreadable records |
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
