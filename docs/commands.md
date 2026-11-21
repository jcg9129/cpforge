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
