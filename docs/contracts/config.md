# Configuration and commands contract

Status: v3 (v2 plus the schema amendment: key matrix §1.1, discovery §1.2,
precedence-preserving merge §2.1, diagnostics §3.2, program-path rules §4.4,
hook switch §8.3), with the expansion amendment of 2026-11-20 (Windows
environment-name equality §4.3, resolver details and launch kinds §4.4,
preparation states §4.5, expansion context §5.2, interpreter rules §5.3)
and the grader-target amendment of 2026-11-20 (file roles §6.2, problem
`entry` §6.4, template destination §6.5, grader tasks §9), settled, not yet
implemented.

**Managed-workspace amendment (v4): settled, not yet implemented.**
`[workspace] ignore` (§7.4) specifies the design in
`../design-workspace.md` §2.4. Amended content is marked *(draft)* below and
is settled but not yet implemented. References labelled *workspace design* point to
`../design-workspace.md`.

**Organisation amendment: draft.** The workflow status vocabulary
`[status] values` (§1.1, §7.5) for `cpf mark` (storage contract §5.9).
Marked *(organisation, draft)*.

**Judging amendment: draft.** The `problem.toml` table `[check]` (§9.1):
judging kind, role specs and their capture, answer policy, scoring,
display profile, interactive settings and the `interactor-test`
declaration; run counts and the judged-test cap (§8.2); problem kinds
(§9). Amended content is marked *(judging, draft)*. References labelled
*checkers design* point to `../design-checkers.md`; *report contract*,
*execution contract* and *storage contract* to the documents in this
directory.

**Stress amendment: draft.** The `problem.toml` table `[stress]` (§9.2):
generator and brute role specs, default budgets and the score threshold;
stress plans (§8.2); the `cpf stress` row of §9. Marked *(stress, draft)*;
the stress contract (`stress.md`) uses it.

**Trust model.** Configuration is code: every build, run and command argv,
shell script and hook executes with the user's rights. The supervisor bounds
time and captured output; it is not a security boundary (design §1). The only
trust decision this contract makes is whether a workspace may run **hooks**
(§8.3).

## 1. Files and layers

| Layer | File | Notes |
|---|---|---|
| 0 built-in | compiled defaults | §6.4 (languages), execution contract §5.5 (caps) |
| 1 user | `$XDG_CONFIG_HOME/cpforge/config.toml` (default `~/.config/cpforge/config.toml`, also on macOS); Windows `%APPDATA%\cpforge\config.toml` | `--config <file>` replaces it for one invocation; hooks are then off (§8.3) |
| 2 workspace | `<root>/cpforge.toml` | |
| 3 problem | `problems/<p>/problem.toml`, table `[config]` only | the outer document carries `schema` and identity (storage contract §3, §9) |

Every file carries `schema = 1` (storage contract §9). The user config file
is classified like the user-editable formats of storage contract §9: an
absent `schema` means 1, a newer one is `too_new` and nothing in the file is
used.

### 1.1 Keys

Every key not listed here is a phase-1 error naming the key and the file.
"Layers" says where a key may appear; elsewhere it is a phase-1 error, also
when its value would be valid. Problem layer means the `[config]` table of
`problem.toml`; the rest of that document belongs to the storage contract
and the problem record, not to configuration.

| Key | Type | Layers | Notes |
|---|---|---|---|
| `schema` | integer | 1, 2 (problem: outer document) | |
| `default_language` | string, a language name | 1, 2 | §6.2 |
| `languages.<l>` | language table | 1, 2 | below; `<l>` is a name (§1.1 names) |
| `build`, `run` | stage table | 1, 2 | global tables, precedence row 2 |
| `commands.<c>` | command table | 1, 2 | §8.1 |
| `compare` | comparator value | 1, 2 | compare spec §5.1; replace-as-unit |
| `retention.{keep_jobs, job_bytes, workspace_bytes, cwd_bytes, build_bytes}` | §7.1 types | 1, 2 | §7.3 |
| `workspace.ignore` | array of patterns | 1, 2 | §7.4 |
| `status.values` *(organisation, draft)* | array of status names | 1, 2 | §7.5; replace-as-unit |
| `hooks.on_switch`, `hooks.after_test` | array of command names | 1, 2 | §8.3 |
| `hooks.trusted_workspaces`, `hooks.enabled` | array of absolute paths; boolean | default user file only | §8.3; an error in a `--config` file |
| `config.language` | string | 3 | §6.2 |
| `config.sources` | array of paths | 3 | §6.2 |
| `config.support_files` | array of paths | 3 | §6.2 file roles |
| `config.solution_files` | array of paths | 3 | §6.2 file roles, §6.5 |
| `config.io` | table `{ input?, output? }` | 3 | reserved file-I/O declaration; §6.2 |
| `config.entry` | non-empty string | 3 | §6.4; overrides the language's `entry` |
| `config.compare` | comparator value | 3 | |
| `config.build`, `config.run` | stage table | 3 | precedence row 5 |

**Language table** `languages.<l>`: `extensions` (array of non-empty
strings without `.` or `/`), `sources` (array, §6.2 rules), `entry` (non-empty
string), `artifacts` (§6.3; exactly one of `file = "<name>"` or `dir = true`
with optional `required`), `template = { path }` (config-relative), `build`
(stage table or `false`), `run` (stage table), `profiles.<p>` (table with
optional `build`, a stage table or `false`, and `run`), platform overlays
(§2).

**Stage table** (build, run, global, profile and problem stages): `argv`,
`shell = { interpreter, script, args }`, `cwd`, `env`, `env_remove`,
`deadline_s`, `stdout_cap`, `stderr_cap`, `limits = { address_space,
job_commit }` (§4.1, §7). **Command table**: the stage fields plus
`needs_build`, `for_each_test`, `repeat`, `keep_going` (booleans except
`repeat`, §8) and `stdin`, plus platform overlays. `stdin` elsewhere is an
error. `run = false` is an error.

**Nested fields.** `argv`, `shell.interpreter`, `shell.args`, `env_remove`,
`extensions`, `sources` and `artifacts.required` are arrays of strings;
`shell.script`, `template.path`, `cwd.path`, `cwd.selector`, `entry` and
`artifacts.file` are strings; `env` is a table of strings or `false`. A
composed `shell` needs `interpreter` and `script`; `args` defaults to `[]`.
`template` needs `path`. `cwd` holds exactly one of `path` or `selector`
(`"run"`, `"problem"`, `"workspace"`). `artifacts` holds `file`, or `dir =
true` with optional `required`; `required` with `file` and `dir = false`
are errors. Unknown nested keys are errors. Paths in this section are
document-root paths: the problem layer's keys are those under `config.`.

**Names** of languages, profiles and commands: non-empty, ASCII letters,
digits, `-` and `_`, starting with a letter; `unix`, `linux`, `macos`,
`windows` are refused as language and profile names (they would collide
with overlay keys) but allowed as command names (§2).

Keys from later designs (`submit`, `archive`, `check` outside `problem.toml`)
are unknown keys in this version.

*(judging, draft)* `problem.toml` `[check]` is not a configuration layer:
it is a table of the problem record whose schema this contract owns
(§9.1). It is read only from the selected problem's `problem.toml`, is
validated in the phases of §3 with ordinary diagnostics (file
`problem.toml`, key path `check.…`), and never appears in layers 1–3.

*(stress, draft)* `problem.toml` `[stress]` is a table of the problem
record in the same way (§9.2), with key path `stress.…`.

### 1.2 Discovery

- User layer: Unix uses `$XDG_CONFIG_HOME` if it is non-empty and
  absolute, else `$HOME/.config` (macOS too); Windows needs a non-empty
  absolute `%APPDATA%`. No usable base directory → a discovery error. The
  default file absent → an empty layer; present but unreadable, malformed
  or `too_new` → error. `--config <file>` absent or unreadable → error, and
  with `--config` the default file is not read at all, its hook settings
  included (§8.3).
- Workspace layer: `cpforge.toml` at the workspace root (found per storage
  contract §2); outside a workspace there is no layer 2 and no layer 3.
- Problem layer: the selected problem's `problem.toml` (selected per storage
  contract §5.7; §9 classification); a problem without a `[config]` table contributes an empty
  layer. Only the selected problem's file is read.
- A minimal workspace (`schema = 1` only, as `cpf init` writes it) is valid.
  A missing language (no problem `language` and no `default_language`) is an
  error only for an operation that needs a target.

## 2. Merge

- Layers merge in order 0 → 3; later wins. Tables merge key by key,
  recursively, **except replace-as-unit nodes**, which a later layer replaces
  whole: every array; `compare`; `artifacts`; `template`; `limits`; `env_remove`; `cwd`
  (`selector` and `path` never mix); and a stage's **command**: setting
  `argv` removes an inherited `shell` and vice versa, and setting either
  form replaces the whole inherited command, also `shell` over `shell`;
  omitted `shell` fields are never inherited.
- **Deletion:** `build = false` in a language (or profile) removes an
  inherited build stage (the language becomes build-free); `env = { NAME =
  false }` **unsets** `NAME`: it is absent from the final environment whatever
  the baseline had (§4.3). The tombstone survives merging until a later or
  more specific source sets `NAME` again. `limits = {}` clears all
  limits. Deletion markers are valid only at these three places.
- **Platform overlays** exist only at `languages.<l>`, `languages.<l>.build`,
  `languages.<l>.run`, `languages.<l>.profiles.<p>`, `commands.<c>`, `[run]`
  and `[build]`. There the keys `unix`, `linux`, `macos`, `windows` are
  reserved sub-tables; elsewhere they are ordinary names (e.g. a command
  named `linux`). No nesting (`unix.linux` is an error). Within **one layer**
  a node is computed as generic → `unix` (Linux and macOS) → the specific OS,
  with replace-as-unit and deletion applied inside that computation; the
  result then merges with the next layer. So a later layer's generic value
  overrides an earlier layer's platform value.
- Overlay sites also include `config.build` and `config.run` (problem
  layer). An overlay table holds what its site may hold except further
  overlay keys: `languages.cpp.windows.run` and
  `languages.cpp.windows.profiles.debug.build` are valid; `unix.linux`,
  `languages.cpp.windows.run.linux` and `languages.cpp.profiles.p.build.windows`
  (no overlay site beneath a profile's stage) are errors.
- **Nested sites.** Within a layer a site's overlays are resolved after
  those of the sites beneath it: first `languages.cpp.run` is computed from
  its generic table, `run.unix` and `run.<os>`; then `languages.cpp` with
  `languages.cpp.unix` and `languages.cpp.<os>` merged over that result. So
  `languages.cpp.windows.run.argv` beats `languages.cpp.run.windows.argv`
  in the same layer.
- **Overlay-only tables.** A table that holds only overlay keys contributes
  nothing on a platform where none of its overlays is active: it behaves as
  if absent, exactly like the `languages.<l>.<os>.build` spelling. So
  `[languages.python.build.windows]` leaves Python build-free on Linux and
  `[commands.x.windows]` defines no command `x` there. An explicitly written
  empty table (`build = {}`, or a `[languages.python.build]` header with no
  keys) is present.

### 2.1 Merge per precedence row

Layers are merged **within** each precedence row of §6.1, never into one
flat table first: a problem `[config.build]` (row 5) stays above a
user-level profile (row 4) whatever their layers. For each stage the rows are
then composed lowest to highest with the same replace-as-unit rules.
Deletion markers survive composition: an `env` tombstone stays until a
higher source sets the name again, and is then shown with both origins.

**Build stage existence.** Only rows 3 and 4 decide whether a build stage
exists. Their contributions are applied in the order row 3 layers 0 → 2,
then row 4 layers 0 → 2; `build = false` is a **reset**: it discards every
row-3/4 contribution before it, and a later build table starts a fresh
stage from its own fields, so fields discarded by the reset do not return.
The merged contribution keeps the fact that a reset occurred, so a reset
followed by a table is not the same as the table alone. Rows 2 and 5 are
**modifiers**: they never create a build stage, are not discarded by a
reset, and apply to the stage if one exists; for a build-free target the
global `[build]` and the problem's `[config.build]` are ignored (shown as
ignored by `cpf config show`). Row 1 defaults (deadlines, caps) apply to any
stage that exists. After the resets, the surviving contributions
are composed with rows 1, 2, 5 and 6 in §6.1 order, and completeness is
validated on that result (phase 2); discarded row-3/4 fields never supply a
missing field.

## 3. Validation phases

| Phase | When | Checks |
|---|---|---|
| 1 raw layer | each file, on load | TOML syntax, known keys, types, placeholder grammar (§5.1), deletion markers only where allowed; mutual exclusivity (`argv`/`shell`, `cwd` forms, artifact forms) checked **twice**: first within each raw table (generic, `unix`, OS, profile) on its own, so one table holding both `argv` and `shell` is an error even if an overlay would replace it; then after the layer's overlays are composed, per platform branch, so a generic `argv` with a Windows `shell` is fine; inactive branches structurally only |
| 2 effective | after full stage composition (§6.1) | required fields and stage completeness on the composed stage, never on partial layers or tables; references (hooks → commands, profiles); cross-field rules (§§5–8) |
| 3 selected plan | when a command plans a job | placeholder availability per stage and test (§5.2), active-platform limits (§7.2), external program resolution (§4.4), sources present (§6.2), run count (§8.2) |
| 4 post-build | after a build's exit 0 | declared outputs (§6.3), generated programs (§4.5) |

Phases 1–3 report **all** offending keys at once, with provenance (§3.1),
before anything runs (also before hooks). An unused language, profile or
command is checked in phases 1–2 only; its tools need not exist.

**Validation platform.** Loading takes an explicit platform (the host by
default; tests inject one). Every overlay branch gets the structural checks
(keys, types, grammar, raw exclusivity); platform-specific lexical rules
(Windows path forms, Windows case-insensitive duplicates) apply to the
generic values and the overlays active on that platform. `limits` values
parse in phase 1 to byte counts ≥ 1 and ≤ `u64::MAX` with checked
arithmetic; page rounding and platform maxima (§7.2) are phase-3 checks for
the active platform.

**Phase-2 universe.** `cpf config check` and every load validate, for the
validation platform: each language through rows 1–4 without a profile and
with each of its profiles; each command from its own layered tables plus
command defaults, and again with hook defaults and restrictions if a hook
names it (hooks are validated even when disabled or untrusted). When a language can be selected,
the selected problem is also validated with that language, the selected
profile and row 5; a missing language is an error only for an operation
that needs a target (§1.2), otherwise target settings stay unresolved. Every language needs a complete run stage; every existing build
stage and every command needs exactly one complete `argv` or `shell`.

### 3.1 Provenance

Every effective leaf and array element records one of
`File { path, line, col }`, `BuiltIn`, `Cli { flag }`,
`Environment { name }`, `Derived { inputs: [Provenance] }` (e.g. a deadline
computed from a published limit, an expanded argv element). `cpf config show
--origin` prints it, plus the platform overlay and profile applied.

### 3.2 Diagnostics and recovery

- Each diagnostic names the file, the key path and, when known, line and
  column; a conflict (two exclusive forms, a duplicate) names both origins.
  Legal overrides and tombstones are provenance, not diagnostics.
- A file with a TOML syntax error, unreadable bytes or a `too_new` schema
  yields that one diagnostic and makes the layer **invalid** (not empty);
  the other files are still checked.
- A field with a phase-1 error leaves an **invalid marker** at its key (a
  container error, at the container's path). Markers keep provenance
  through overlay and row composition, also when a later value overrides
  them. Phase 2 skips each check whose inputs include a marker, so no
  completeness error is derived from an invalid field; all other checks
  run, and all diagnostics of phases 1 and 2 are reported together.
- Any phase-1 or phase-2 error prevents plan construction; a phase-3 error
  prevents executing the selected plan. Warnings (e.g. a trust
  canonicalisation failure, §8.3) never make configuration non-executable.
- An invalid file leaves a layer-wide **unknown-input marker**: phase-2
  checks whose result depends on that layer (a reference that could be
  defined there, a stage that could be completed there) are skipped; raw
  validation of other files and independent checks continue.
- Provenance (§3.1) records, per key, the ordered events set, replace,
  delete (tombstone), reset (`build = false`) and ignored, including the
  values removed.

### 3.3 Consumer interface

Loading, platform composition and target resolution are separate steps.
Results expose diagnostics, whether each field was explicitly present,
provenance with its events, and deferred values. Target resolution takes
the language or problem, the profile and the CLI overrides, and returns the
declarative stages, the context requirements (§8.1) and the effective target
settings (`sources`, artifacts, `entry`, template, compare). Loading and
phases 1–2 never probe tools, read sources or allocate job paths.

## 4. Execution specs

```toml
[languages.cpp.build]
argv = ["g++", "-std=gnu++23", "-O2", "{sources}", "-o", "{artifact}"]
deadline_s = 60
env = { LC_ALL = "C" }
env_remove = ["CPATH"]
```

### 4.1 Stage fields

`argv` (non-empty array) **or** `shell` (§5.3); `cwd` (§4.2); `env` (table of
string or `false`); `env_remove` (array of names); `deadline_s` (§7.1);
`stdout_cap`, `stderr_cap` (sizes; execution contract §5.5 defaults); `limits` (§7.2);
`stdin` (commands only: `"null"` default, `"test"` with `for_each_test`).

### 4.2 Typed paths

| Type | Base | Used for |
|---|---|---|
| config-relative | directory of the file that defined it | `template.path`, a literal program path in `argv[0]`, a literal `cwd.path` (§4.4 order) |
| problem-relative | the problem directory | `sources` |
| output-relative | the build's fresh output root (storage contract §6.1) | `artifacts.file`, `artifacts.required` |
| cwd selector | symbolic | `cwd = { selector = "run" }` (default: the run's fresh `cwd/`, storage contract §7), `"problem"`, `"workspace"`; or `cwd = { path = "…" }` (config-relative); every non-`run` cwd marks the run `shared_cwd` (storage contract §7) |

- `~` is expanded only as a leading `~/` (Windows also `~\`) in
  config-relative paths. A path in a CLI flag is relative to the current
  directory. Windows drive-relative (`C:foo`) and root-relative (`\foo`)
  forms are errors; UNC and absolute drive paths are allowed in
  config-relative paths only.
- Config-relative paths are joined lexically at load (phase 1), never
  canonicalised; problem- and output-relative paths are joined at plan time.
  Output-relative names must be relative, without `..`, root or drive prefix
  (phase 1).

### 4.3 Environment

Built for each run from one baseline, in this order:

1. **Baseline:** `cpf`'s own environment, captured once at job start and
   shared by every run of the job.
2. Remove every name in the effective `env_remove` (case-insensitive on
   Windows).
3. Apply the effective `env` map: set each name; a `false` value (tombstone,
   §2) unsets the name, so it is absent from the final environment even if
   the baseline had it. Across layers later wins, compared
   case-insensitively on Windows; two keys of **one** map differing only in
   case on Windows are an error.
4. Inject `CPF_*` variables (§8.3) last; configuration cannot override them.

**Windows name equality.** Wherever this section compares names on Windows
(removal, tombstones, later-wins override, same-map duplicates), it uses the
operating system's ordinal case-insensitive comparison, the comparison
Windows and Rust's `Command` apply when installing an environment, not an
ASCII-only fold; the winning spelling is installed. Only the reserved `CPF_`
prefix check below stays ASCII case-insensitive. Unix compares names
byte-exactly.

Program resolution (§4.4) reads `PATH` and, on Windows, `PATHEXT` from the
run's environment after steps 1–3, so a stage's `env` edits decide where its
program is searched.

Names containing NUL or `=` (Windows: also empty names), and values containing
NUL, are errors, and so is a name in `env` or `env_remove` that starts with
`CPF_` (ASCII case-insensitive on Windows), all in phase 1. Inherited values
stay native strings. The supervisor installs
exactly this map (`env_clear` + set, execution contract §2).

### 4.4 Program resolution

`cpf` resolves programs itself; Rust's search is never used.

**Order for `argv[0]` and `cwd.path`**, after a leading `~/` is expanded
(§4.2): (1) an element with no placeholder that is a relative **path** is
rebased against its defining file's directory at load; for `argv[0]` a path
is an element containing a separator (`/`; on Windows also `\`), so a bare
name such as `g++` is a lookup token and never rebased, while `cwd.path` is
always a path; (2) an element that **starts** with a placeholder is never
rebased; (3) a **relative** element that contains a placeholder **not** at
its start (e.g. `./{problem_id}/tool`) is an error at load: start it with a
placeholder (`{workspace}/…`) or write an absolute path, which may contain
placeholders anywhere and is never rebased; (4) expansion (§5);
(5) classification of the expanded `argv[0]`: contains a separator → path,
else `PATH` lookup. An empty result is an error. "Rebased at load" in §4.2
means exactly rule (1). Other `argv` elements are never rebased. `shell.interpreter[0]` is literal: leading `~/` expansion, rebasing by
rule (1) and program resolution apply, but placeholders are neither
recognised nor expanded in it (§5.1); a `{` or `}` anywhere in the interpreter
is a phase-1 error, so `{{` is not an escape there; its resolved path is absolute. After
expansion, an `argv[0]` containing a separator and `cwd.path` must be
absolute; a relative result (e.g. from `{problem_id}/tool`) and the
Windows drive-relative and root-relative forms are phase-3 errors.

| Case | Outcome |
|---|---|
| `PATH` missing or empty | only explicit paths resolve; a bare name → not found |
| empty or relative `PATH` entry | skipped, recorded in the plan; current directory never searched |
| Windows `PATHEXT` missing or empty | `.COM;.EXE;.BAT;.CMD` |
| Windows extension comparison | case-insensitive |
| Windows name with **any** extension (`x.exe`, `x.ps1`, `x.tar.gz`) | tried as is only, never with `PATHEXT` appended |
| Windows name without an extension | each `PATHEXT` extension in order (bare name not tried) |
| Windows explicit path without an extension | as is, then each `PATHEXT` extension |
| Windows launchable | only `.exe`, `.com`, `.bat`, `.cmd` (case-insensitive) |
| Windows program name ending in `.` or a space (`tool.`, `tool.cmd `), as written or after expansion | phase-3 error; never trimmed |
| Windows malformed `PATHEXT` entry (empty, or not `.` followed by a non-empty name without separators) | skipped and recorded |
| Unix launchable | regular file (symlinks followed) with execute permission for the **effective** user, checked for the effective credentials and honouring ACLs, not merely any execute bit: when real and effective IDs are equal, `access(X_OK)`; otherwise Linux `faccessat2(…, X_OK, AT_EACCESS)` and macOS `faccessat(…, AT_EACCESS)`; where only an emulation is available (older Linux without `faccessat2`, whose `AT_EACCESS` emulation ignores ACLs) the mode-bit result is used and recorded as approximate |
| candidate that exists but is not launchable (directory, no permission, other extension such as `.ps1`) | skipped, search continues; if no launchable match is found, the error names the skipped candidates and why |
| several matches | first launchable match wins |
| nothing found | phase-3 error; no fallback, Rust's search never used |

The absolute result goes into the spec and the record, together with the
skipped `PATH` entries and candidates. A resolved program also carries its
**launch kind**: `native`, or on Windows `batch` for a `.bat`/`.cmd`
result (case-insensitive). Launch kind is independent of whether the program
is external or generated (§4.5): a generated program can be a batch file.

- `.bat`/`.cmd`: launched through Rust's batch escaping (the Rust 1.81 fixes
  are within MSRV 1.85). An argument it refuses is the launch failure
  `UnsafeBatchArgument`, never passed unescaped; a launch error is classified
  as `UnsafeBatchArgument` only when argument rejection is established (for
  example a CR or LF in an argument, checked before spawn), never from an
  `InvalidInput` error alone. Batch execution runs `cmd.exe` internally: the
  one documented exception to "no implicit shell".
- The launcher must start exactly the resolved path. Rust's own resolution
  may append `.exe` to an explicit path that lacks it (with both `tool.cmd`
  and `tool.cmd.exe` present, Rust 1.85 can start the latter); the Windows
  launcher (gate task B4a) must prevent this while keeping Rust's batch
  escaping and creation-time containment, and its tests include that
  collision. Configuration expansion only selects the path and launch kind.

### 4.5 Staged materialisation

Plans are symbolic until each launch. Phase 3 resolves external programs and
allocates output paths. A **generated** program is any expanded program path
under the run's artifact directory (`{artifact}`, or a path built from
`{artifact_dir}`), decided by lexical component-wise containment after
expansion (`.` and `..` components are an error in a generated path; Windows
components compare case-insensitively; never a string-prefix test). This is
scheduling classification, not a security boundary. A generated program
needs a known producer (the target's build) and a stage cannot be its own
producer. It is checked (exists, regular file, launchable and launch kind per
§4.4) after its producer succeeded (phase 4) and before its consumer
launches; a successful producer exit alone is not enough, and the build's
declared-output check (§6.3) is a separate check. Every other program is
resolved in phase 3, **for every stage of the job before anything runs**.
A shell interpreter is never generated (§5.3).

Expansion yields a **prepared stage**, which cannot be launched; resolving its
program (at once, or after the producer for a generated program) yields a
**resolved stage**: program path, launch kind, argv, absolute cwd, the
environment after §4.3 steps 1–3, and the resolution record. Job admission
adds the `CPF_*` variables, stdin, output sinks and normalised limits to form
the immutable `ResolvedExecutionSpec` (execution contract §2), computes its
digest and enforces its size limit; a prepared stage is never persisted or
hashed as a spec. A fully resolved `ResolvedExecutionSpec` exists at each
launch boundary (execution contract §3.4). Preflight does not guarantee that
files stay unchanged until spawn: an executable checked during preparation
may still fail at spawn.

## 5. Placeholders

### 5.1 Grammar and fields

- Recognised only as `{name}` with a name from §5.2; `{{` and `}}` are literal
  braces; a lone brace is an error.
- Expanded in `argv` elements, shell `args` and `cwd.path`; **never** in env
  values, the shell interpreter or script, or CLI extra args (appended
  verbatim, §8.1).
- Program position (`argv[0]`) and `cwd.path` must expand to exactly one
  scalar; multi placeholders there are an error (phase 1), even when one
  source exists.
- A **multi** placeholder must be a whole element and expands to 0…n
  elements; scalars may be embedded (`-o{artifact}`). Single pass, never
  rescanned; values are native strings; a NUL after expansion is an error.

### 5.2 Table

| Placeholder | Kind | Available when |
|---|---|---|
| `{sources}` | multi | build, run, command with `needs_build` |
| `{source}` | scalar | as `{sources}`, with exactly one source |
| `{artifact}` | scalar | build, run, command with `needs_build`; target has a `file` artifact |
| `{artifact_dir}` | scalar | build, run, command with `needs_build`; target has a build |
| `{entry}` | scalar | the target has an effective `entry` (problem `config.entry`, else the language's) |
| `{problem_dir}`, `{problem_id}` | scalar | a problem is selected (all stages, hooks) |
| `{workspace}` | scalar | inside a workspace (all stages, hooks) |
| `{test_input}` | scalar | command with `for_each_test` |
| `{test_answer}` | scalar | command with `for_each_test` and the test has an answer (else plan error for that test) |
| `{iteration}` | scalar, 1-based | command with `repeat` |

In **build**, `{artifact}` and `{artifact_dir}` are the allocated output paths
the build writes; **path availability ≠ output existence**: outputs are checked
only in phase 4.

A placeholder whose **declaration** is missing is a phase-2 error (§8.1),
because it is decided by the configuration alone: in a command, `{sources}`,
`{source}`, `{artifact}` and `{artifact_dir}` without `needs_build`,
`{test_input}` and `{test_answer}` without `for_each_test`, `{iteration}`
without `repeat`; in a hook, anything but `{problem_dir}`, `{problem_id}` and
`{workspace}`. Every other unavailability (it depends on the invocation, the
target or the test) → phase-3 error naming the stage and placeholder.

Availability comes from what the invocation **declares**, never from which
values the caller happens to supply: the stage kind and, for a command, its
`needs_build`, `for_each_test` and `repeat`. A value supplied for an
undeclared placeholder does not make it available. Distinct causes stay
distinct in errors: no selected problem, not inside a workspace, a target
without a `file` artifact, a test without an answer. `{source}` is derived
from `{sources}`; an empty `sources` list is available and expands
`{sources}` to zero elements, while `{source}` then fails. *(draft)* In a
managed workspace the source values are the captured copies supplied by job
admission; expansion never falls back to the originals.

### 5.3 Shell adapter

```toml
[commands.bench]
needs_build = true
for_each_test = true
shell = { interpreter = ["bash", "-c"], script = 'for i in 1 2 3; do "$1" < "$2" > /dev/null; done', args = ["{artifact}", "{test_input}"] }
```

- `interpreter` must be exactly `[<p>, "-c"]` where `<p>` resolves (§4.4) to a
  program whose basename (without `.exe`) is `sh`, `bash`, `dash`, `zsh` or
  `ksh`; anything else is an error (phase 1 for the form, phase 3 for the
  resolved name). The check uses the basename of the selected path, without
  following symlinks (`/bin/sh` linked to `dash` is `sh`); on Windows it is
  case-insensitive and a trailing `.exe` is stripped. The interpreter is an
  external program resolved in phase 3; one under the artifact directory is a
  phase-3 error.
- The argv is `<p> -c <script> cpf <args…>`: `$0 = cpf`, `$1…` the expanded
  args. `script` is literal; values reach it only as positional parameters,
  which the script must quote. This prevents substitution into the script
  text, not unsafe use inside it: scripts and explicit interpreter commands
  are trusted code.
- `argv` is never passed to a shell (batch files excepted, §4.4).

## 6. Languages and targets

### 6.1 Precedence of a stage spec

Lowest → highest; within each row, layers merge per §2:

| # | Source |
|---|---|
| 1 | built-in stage defaults (execution contract §5.5 caps, §7.1 deadlines) |
| 2 | global stage tables `[build]`, `[run]` |
| 3 | `languages.<l>.build` / `.run` |
| 4 | the selected profile `languages.<l>.profiles.<p>.build` / `.run` |
| 5 | problem stage overrides `[config.build]`, `[config.run]` |
| 6 | CLI flags (`--deadline`; `--profile` selects row 4) |

Specificity outranks layer: a user-level profile value beats a workspace
language value. The same order applies to `env`, `env_remove`, `cwd`, caps
and limits.

### 6.2 Target selection

- Language: problem `[config] language`, else `default_language` (layers
  1–2), else unresolved: an error only for an operation that needs a
  target (§1.2).
- `sources`: an explicit, ordered array of problem-relative paths, no
  globbing; default: the language's `sources`. Each path is validated in
  phase 1: non-empty, relative, components separated by `/` only (a `\` is
  an error on Windows), no empty, `.` or `..` component, no NUL. Duplicates
  are an error, compared component-wise with the same case rule as §7.4
  (ASCII case-insensitive on Windows and macOS), so `a.cpp` / `A.cpp` on
  those platforms are duplicates. In phase 3 each path must name an
  existing regular file reached without a link component and outside the
  reserved trees (storage contract §5.4 path rules); otherwise a phase-3
  error, and so are two sources that name the **same file** (same device
  and inode on Unix, same volume serial number and file ID on Windows),
  which catches filesystem aliases the textual comparison misses (APFS
  normalisation variants, case variants). The capture destination is
  `inputs/src/` joined with the same components, so it stays inside the
  capture root; destinations are created exclusively (storage contract
  §7), so a remaining alias fails capture and never overwrites a copy.
- **File roles** (grader-linked targets, e.g. a contestant `solution.cpp`
  compiled with a provided `grader.cpp` that includes `grader.h`). Three
  problem-layer lists, all ordered problem-relative paths with the same
  phase-1 rules as `sources`:
  - `sources`: the **tool inputs**, the only files `{sources}` / `{source}`
    expand to (compilation units; for a build-free Python target, the
    script to execute, e.g. the grader).
  - `support_files` (default `[]`): files the target needs beside its
    sources but never passed as tool inputs (headers, imported modules).
    The **captured files** are `sources` followed by `support_files`; both
    get the phase-3 checks above (existing regular file, no link component,
    outside reserved trees, same-file aliases), and a path in both lists,
    compared as for duplicates, is a phase-2 error, never silently merged.
  - `solution_files` (default: `sources` followed by `support_files`): the
    files the contestant edits; every member must be one of the captured
    files (phase 2) and duplicates are an error. It names no extra
    capture and makes no file read-only: files outside it are **provided**
    files, a convention used by `cpf add` (§6.5) and importers, not
    enforced. An explicitly empty `solution_files` is valid; a template
    then cannot be materialised.
  Roles live only in the problem layer: a language cannot know which files
  a judge provides. They change neither placeholder meaning nor cardinality:
  `{source}` still requires exactly one member of `sources`.
- *(draft)* In a managed workspace, sources are **captured** into the job
  (storage contract §7) and every `{sources}` / `{source}` expansion names
  the copy; outside the amendment they are read in place. Test data is
  always frozen.
- **Reserved file-I/O declaration:** problem `[config]` alone (layer 3) accepts
  `io = { input = "<name>", output = "<name>" }`. Both fields are optional,
  but at least one is required; unknown fields are errors. Each name is one
  relative portable component under storage contract §§2–2.1, validated by
  the same portable-component rules on every platform. These are literal
  names, without placeholder expansion or config-relative path resolution.
  Phase 1 validates the table shape, string types and names, with ordinary
  diagnostics and invalid markers naming the file, key and source location
  (§§3.1–3.2). A valid, present `io` is rejected in phase 2 with the dedicated
  diagnostic **"file-I/O problems are not supported in this version"** at
  `config.io`. This applies to every command that loads that problem's
  configuration, whether or not a language is resolved, and with any `cwd`,
  shared or otherwise. It is a configuration error (exit 7), before job
  admission; no job directory or sequence allocation occurs. Read-only
  `cpf config show`, `cpf config check` and `cpf doctor` report it like any
  other phase-2 error (exit 7). Configuration errors precede an unsupported
  execution-platform refusal (report contract §5.2); a valid configuration
  still refuses job execution on Windows with exit 10 before admission.
- `multi-file` is a problem kind (§9), unrelated to several source files.

### 6.3 Artifacts

- `artifacts = { file = "<name>" }`: `{artifact}` = `<output root>/<name>`;
  on Windows `.exe` is appended when `<name>` has no extension.
- `artifacts = { dir = true, required = ["<rel>", …] }`: only
  `{artifact_dir}`; `{artifact}` is an error for this target.
- Build success (phase 4) = exit 0 **and** every declared output (the file,
  or every `required` path) exists as a regular file beneath the output
  root, not through a link. A directory artifact whose `required` is empty
  after defaults (§6.4) is a phase-2 error, so a no-op build cannot pass.
- A language without a build stage (or with `build = false`) is build-free:
  `{artifact}` / `{artifact_dir}` are errors and no artefact is checked.
- Output size: `build_bytes` (§7.3).

### 6.4 Built-in languages

```toml
[languages.cpp]
extensions = ["cpp", "cc", "cxx"]
sources = ["main.cpp"]
artifacts = { file = "main" }
[languages.cpp.build]
argv = ["g++", "-std=gnu++23", "-O2", "-Wall", "{sources}", "-o", "{artifact}"]
[languages.cpp.run]
argv = ["{artifact}"]

[languages.python]
extensions = ["py"]
sources = ["main.py"]
build = false
[languages.python.run]
argv = ["python3", "-B", "{source}"]
[languages.python.run.windows]
argv = ["python", "-B", "{source}"]

[languages.java]
extensions = ["java"]
sources = ["Main.java"]
entry = "Main"                        # may be package-qualified: "a.b.Main"
artifacts = { dir = true }            # required defaults from entry
[languages.java.build]
argv = ["javac", "-d", "{artifact_dir}", "{sources}"]
[languages.java.run]
argv = ["java", "-cp", "{artifact_dir}", "{entry}"]
```

The **effective entry** is problem `config.entry` if set, else the
language's `entry`; `config.entry` has the language `entry` rules (phase 1:
non-empty, no NUL). For Java, `required` defaults to the class file derived
from the effective entry (dots →
path separators, plus `.class`: `a/b/Main.class`). The derived path must
satisfy the output-relative rules (§4.2: relative, no empty, `.` or `..`
component, no root or drive prefix); otherwise a phase-2 error located at
the effective entry, so `entry = "/tmp/Grader"` is refused before any build. All other classes the
compiler writes (inner, auxiliary, other packages) stay in the output root,
which is the class path. Python's `-B` keeps ordinary imports from writing
bytecode beside the captured files; it does not stop a program from writing.

Grader-linked targets with the built-ins, no stage overrides needed:

```toml
# C++: grader.cpp holds main and includes grader.h
[config]
sources = ["solution.cpp", "grader.cpp"]
support_files = ["grader.h"]
solution_files = ["solution.cpp"]

# Python: the grader runs and imports solution.py from its own directory
[config]
language = "python"
sources = ["grader.py"]
support_files = ["solution.py"]
solution_files = ["solution.py"]

# Java: the grader's class is the entry; required becomes Grader.class
[config]
language = "java"
sources = ["Solution.java", "Grader.java"]
solution_files = ["Solution.java"]
entry = "Grader"
```

Captured files keep their relative layout. That makes a C/C++ quoted
`#include` of a sibling (searched first in the including file's directory)
and a Python import of a module beside the executed script (`sys.path[0]` is
the script's directory) resolve as in the problem directory. It does not
make Java dependencies discoverable: `javac` and `java` search only their
source and class paths, so **every Java compilation unit must be listed in
`sources`**, and `support_files` holds only non-source resources there. A
grader that reaches files by
other paths (an include directory elsewhere, a path relative to the working
directory) needs its own stage settings, and the run's cwd is never switched
to the problem directory to make it work.

### 6.5 Profiles and templates

`[languages.<l>.profiles.<name>]` with optional `build` and `run`, selected by
`--profile`; unknown → phase-2 error; the record names it. `template = { path
= … }` per language (config-relative), used by `cpf add` without overwriting
(storage contract §5.2). The template is copied to the target's single
**solution file**: `cpf add` with a template requires exactly one effective
`solution_files` member (§6.2; by default the sources, so a plain target
needs exactly one source), and zero or several are an error before
anything is published (a mapping of several template files is a later
design). The destination is resolved from the configuration the new
problem will have: the inherited layers plus the prospective record's
`[config]` (for local `cpf add`, only what `add` itself writes, i.e.
`--language`, so its solution file is the language's single default source;
an importer writes the file roles into the record first). Importers that
supply a contestant stub write the stub there instead of the template: one
producer per destination, never a second write. Checking
configuration alone does not report it.

## 7. Deadlines, limits, retention

### 7.1 Durations and deadlines

- Numeric fields; validation order: type → unit parse → finiteness
  → sign → **rounding → range** → checked conversion, so the minimum applies
  to the rounded value (`0.0005` s rounds to 1 ms and passes; `0.0004` s rounds
  to 0 and fails). Negative values, NaN, infinities and
  overflow are always errors.

| Field | Type | Unit | Min | Max | Zero | Rounding |
|---|---|---|---|---|---|---|
| `deadline_s`, durations | number or `"500ms"`/`"2s"`/`"1m"` | s | 1 ms | 24 h | error | half up to ms |
| `stdout_cap`, `stderr_cap` | integer or size string | bytes | 1 | 1 TiB | error | none (integers) |
| `job_bytes`, `workspace_bytes`, `cwd_bytes`, `build_bytes` | integer or size string | bytes | 1 | 1 PiB | error | none |
| `limits.address_space`, `limits.job_commit` | integer or size string | bytes | 1 page | platform max | error | up to page size (execution contract §7.2) |
| `keep_jobs` | integer | – | 1 | 100 000 | error | – |
| `repeat` | integer | – | 1 | 1000 | error | – |

  Size strings: `KiB`/`MiB`/`GiB`/`TiB` (binary), `kB`/`MB`/`GB`/`TB`
  (decimal); a fractional size is an error.
- **Unset is distinct from a default.** Defaults per stage: run = published
  time limit × 2 + 1 s (checked; only when the problem publishes a finite
  limit), else 10 s; build 60 s; command 60 s; hook 30 s.
  A derived run default that is unusable (above 24 h, or overflowing) is
  an error only when no higher row sets `deadline_s`.
- `--deadline` sets only the run stage of `cpf test` and `cpf run` (row 6,
  §6.1), not builds, commands or hooks.
- The deadline starts when creation returns (execution contract §3.4); queued runs do not
  consume it.

### 7.2 Local limits

```toml
[run]
limits = { address_space = "1GiB" }     # explicit local mechanism (execution contract §7.2)
```

`address_space` (Linux) and `job_commit` (Windows). Validated for the active
platform only (phase 3): a limit that is active here but unsupported is an
error; other platforms' overlay values are checked structurally. `limits =
{}` clears. Published judge limits never become local limits implicitly.

### 7.3 Retention

`[retention]`: `keep_jobs` (integer ≥ 1, default 20); `job_bytes` (default
1 GiB, exact per job); `workspace_bytes` (default 4 GiB, soft target);
`cwd_bytes` (default 256 MiB per run's `cwd/`) and `build_bytes` (default
1 GiB per build output root), both measured after the run or build, not live
limits (storage contract §6.3–6.4; beyond them `CwdOverflow` / `BuildOutputTooLarge`).
Classification: `CwdOverflow` is a **diagnostic** recorded on the run,
never a verdict and never a command failure (storage contract §6.4); `BuildOutputTooLarge`
is a **build failure** (the build stage failed; nothing runs from it), not an
infrastructure failure.

### 7.4 Workspace ignore patterns *(draft)*

`[workspace] ignore = ["*.swp", "scratch/**"]`, allowed in layers 1 and 2
(user, workspace); an array, so a later layer replaces it (§2). Patterns
are matched against **problem-relative** paths with `/` separators:

- `*` matches any run of characters except `/`; `?` one character except
  `/`; `**` as a whole path component matches zero or more components;
  no other metacharacters, no negation, no character classes (a `[`, `]`,
  `!` or `\` is an error in phase 1).
- A pattern without `/` matches the final component at any depth; one with
  `/` is anchored at the problem directory. Empty patterns, a leading or
  trailing `/`, an empty (`a//b`), `.` or `..` component, a `**` that is not
  a whole component (`ab**`, `**b`) and NUL are errors.
- **Units**: matching is per path component. A component that is
  valid UTF-8 is matched per Unicode scalar value (`?` = one scalar). A
  Windows name is decoded from UTF-16 into **code points**, where a lone
  surrogate counts as one code point; a non-UTF-8 Unix component
  is matched per byte. Such units can match only wildcards (and, for
  bytes, their own ASCII values); literal pattern text never matches a lone
  surrogate or a non-UTF-8 byte ≥ 0x80.
  Patterns themselves are UTF-8. Comparison is exact on Linux and
  ASCII-case-insensitive on Windows and macOS; no Unicode normalisation.

Ignore only changes visibility in `cpf status`; it never makes a file
deletable (workspace design §2). `cpf ignore <pattern>` validates a pattern and prints
the line to add; it never edits a configuration file.

### 7.5 Workflow status vocabulary *(organisation, draft)*

`[status] values = ["wa", "upsolve-later"]`, allowed in layers 1 and 2;
an array, so a later layer replaces it whole (§2): a workspace array
replaces the user's, and the user's applies only when the workspace sets
none. `[status]` holds no other key. Each element is a status name
(storage contract §5.9.2: 1–32 ASCII characters from `[a-z0-9-]`, first a
letter, last not `-`); at most 32 elements; no duplicates; no built-in
name (`new`, `in-progress`, `solved`, `skipped`). Each violation is a
phase-1 diagnostic at `status.values[<i>]`; an absent key means no custom
values.

The vocabulary is the built-in names in that order followed by the
effective `values` in order. Only `cpf mark` uses it (storage contract
§5.9.4): it reads layers 0–2 and refuses (7) only when this key cannot be
determined. Readers (`ls`, `status`, `search`) never load it: a stored
status outside the vocabulary is still a valid value. `cpf config show`
shows the effective array with provenance like any array.

## 8. Commands, repeat, hooks

### 8.1 Commands

```toml
[commands.time]
argv = ["{artifact}"]
needs_build = true
for_each_test = true
repeat = 5
```

- `cpf cmd <name> [-- extra…]` runs `[commands.<name>]` through the job
  engine and supervisor, never under the workspace lock; extra args are
  appended verbatim (to `argv`, or to shell `args`).
- `needs_build = true` (default false): the job builds the selected target
  once first (outside the test × repeat product). The target placeholders
  (`{sources}`, `{source}`, `{artifact}`, `{artifact_dir}`) are available
  for planning at once: their paths are allocated before the build and the
  command's stage is prepared with them (§4.5); only the outputs' existence
  and a generated program's check wait for the build to succeed. Using them
  without `needs_build` is a phase-2 error (§5.2).
- `for_each_test = true`: one run per enabled test; stdin defaults to the
  test's frozen input (storage contract §7), `stdin = "null"` overrides. `stdin = "test"`
  without `for_each_test` is an error.
- **Context requirements** are derived from **all** fields: a selected
  problem is needed iff any placeholder of §5.2 needs one, `cwd.selector =
  "problem"`, `needs_build` or `for_each_test`; a workspace iff any of those,
  `{workspace}` or `cwd.selector = "workspace"`. Otherwise the command needs
  no problem. In the CLI foundation every `cpf cmd` still needs a workspace,
  because its job directory lives there (storage contract §6): outside a
  workspace it is refused before admission (workspace class, exit 8).
  Running such commands outside any workspace is a later design.
- **Exit status of `cpf cmd`:** selected by report contract §5.2, whose
  precedence governs. In its terms: usage 2; forced cancellation per report
  contract §5.3; an internal failure 70; a delivery failure of
  `--json`/`--events` 11; any recorded cancellation (including one after
  every run completed) per report contract §5.3; plan errors 7; workspace
  errors 8; job verdict `infrastructure` 10; a failed build (`needs_build`)
  3; a run failure (§8.2) 1; otherwise, when every planned run succeeded, 0.

### 8.2 Repeat and run counts

- `repeat = N`, 1 ≤ N ≤ 1000. Test-major order: test 1 iterations 1…N, then
  test 2, … Each iteration is a separate run with a fresh `cwd/`.
- **Failure** = non-zero exit, signal, deadline, output cap (`OutputBudget`),
  local-limit outcome, `NotStarted`, or any infrastructure outcome (execution contract §6.4).
  The first failure stops the whole command (remaining runs have coverage
  `skipped{stop_policy}`, report contract §2.1) unless `keep_going = true`.
  Cancellation stops before the next run (execution contract §6.4); a
  cancellation records remaining runs as `skipped{cancelled}`.
- `for_each_test` with zero enabled tests is a phase-3 error.
- Aggregates are **per test**: min, median (even count: lower middle) and max
  wall time over successful iterations.
- Planned runs count only hooks allowed by trust, the switch and the depth
  guard at plan time.
- Planned runs = builds + tests × repeats + hooks, computed with checked
  arithmetic in phase 3; more than **10 000** is a plan error (execution contract §6.2
  report bound).
- *(stress, draft)* **Stress plans** (`cpf stress`, stress contract §6.1):
  the planned runs are the builds only: the solution build and one role
  build per built role with a build stage, in the order solution, checker,
  validator, interactor, generator, brute (≤ 6). Iterations and their runs
  are not planned runs and never count towards the 10 000; they are bounded
  by the iteration and time budgets (§9.2, stress contract §5). The
  judged-test cap below does not apply (a stress job has no tests), and the
  report reservation is the fixed 2 × B_S of report contract §3.7. No hook
  is planned for a stress job.
- *(judging, draft)* **Judged plans** (`cpf test` on a problem with a
  `[check]` role, §9.1). Every role run and role build is a planned run
  and counts towards the 10 000:
  - builds: the solution build (if the target has one) and one **role
    build** per built role whose language has a build stage (§9.1.2), in
    the order solution, checker, validator, interactor;
  - per selected test, in test-major order: the validator run (if a
    validator is declared), then the solution run, then the checker run
    (kind `checker`); for kind `interactive`: the validator run (if
    any), the solution run and the interactor run (one session), then the
    follow-up checker run (checking `checker` or `interactor+checker`).
    A test therefore plans 1–3 runs (batch) or 2–4 runs (interactive);
  - hooks as above.

  A **judged test** is a selected test of a problem whose `[check]`
  declares a checker, a validator or kind `interactive`. A job with more
  than **1 000** judged tests is a plan error ("at most 1 000 tests per
  job on a problem with a checker, validator or interactor; select fewer
  with `--case`"), and so is a plan whose report reservation (report
  contract §3.7: 2 × the report bound of the plan) exceeds 256 MiB. Both
  are phase-3 errors (exit 7), checked after the run count, before
  admission.

### 8.3 Hooks

```toml
# workspace or user layer
[hooks]
on_switch = ["notify"]          # names of [commands.*]
after_test = ["log"]
```

```toml
# default user config file only (may also hold on_switch / after_test)
[hooks]
trusted_workspaces = ["/example/cp"]   # absolute workspace roots
enabled = true                         # master switch, default true
```

- **Trust:** hooks run only if the workspace root is listed in
  `hooks.trusted_workspaces` of the **default user config file** (§1), read
  from that file directly, not from the merged configuration. When
  `--config` replaces the user layer, hooks are disabled for the invocation
  (stated in the output). `trusted_workspaces` or an `enabled` key in any
  other layer is a phase-1 error.
- **Switch:** `hooks.enabled` (boolean, default `true`) in the default user
  config file; `false` disables every hook in every workspace, trusted or
  not. Like `trusted_workspaces` it is read from that file directly.
- **Identity**: at check time both each listed path and the workspace
  root are canonicalised (`realpath`; Windows `GetFinalPathNameByHandle`) and
  compared for exact equality of the canonical forms (Windows
  case-insensitively; macOS by canonical form only). A canonicalisation
  failure → untrusted, with a warning. Trust is path-based: a directory
  replaced at a trusted path stays trusted (documented).
- Trusting a workspace authorises its hooks, its command shadowing and any
  future change to its configuration. An untrusted workspace still runs its
  configured build, run and command specs on `cpf test`/`cmd`: workspace
  configuration is code.
- **Hook context:** references are validated in phase 2 as hooks: only
  problem-identity placeholders (`{problem_dir}`, `{problem_id}`,
  `{workspace}`); the keys `for_each_test`, `repeat` and `needs_build` are
  errors on a command used as a hook when **present**, whatever their value
  (`repeat = 1` included). The triggering command captures the problem
  identity and paths **during its metadata operation** and passes that
  context to the hook, so a concurrent switch does not change what the hook
  sees.
- `on_switch` runs after a successful switch; `after_test` in report phase H
  (report contract §2.9), after fact publication, when the primary verdict
  is not `cancelled`, `infrastructure` or `internal_error` and no
  cancellation was requested. The hook plan is captured with the triggering
  event.
- An `after_test` hook's ordinary failure (a failed, cancelled or skipped
  hook) is a warning in the triggering report; a hook internal error is an
  internal failure of the job (report contract §2.6, status 70).
  `on_switch` hooks run as a job of kind `hook` (report contract §2.10). Its
  ordinary failures (verdicts `wall_deadline`, `output_budget`, `signal`,
  `nonzero_exit`, `infrastructure`) and its admission failures of class
  `workspace` or `config` are warnings and leave the status of `cpf switch`
  at 0; a cancellation selects the cancellation status and an internal
  failure (verdict `internal_error` or a caught panic) selects 70, by the
  precedence of report contract §5.2.
- Hooks run after the triggering command released the workspace lock,
  through the supervisor, in the listed order. Effective deadline: the
  referenced command's own `deadline_s` if set, else the hook default (30 s,
  §7.1); other settings as for that command in hook context. An ordinary
  hook failure is a warning in the triggering report (above).
  Cancelling the triggering command cancels hooks not yet started.
- `CPF_HOOK_DEPTH=1` is injected last (§4.3); a `cpf` started with depth ≥ 1
  or a malformed value runs no hooks. This prevents accidental recursion;
  hook code can remove the variable, so it is not a guarantee. Hooks also
  receive the captured identity, injected the same way: `CPF_WORKSPACE`,
  `CPF_JOB_ID`, and for a problem `CPF_PROBLEM_ID` and `CPF_PROBLEM_DIR`.
- Read-only commands (`ls`, `current`, `config`, `doctor`, `status`) never
  run hooks.

## 9. Problem kinds

| Operation | batch | answer | interactive | multi-file | unknown |
|---|---|---|---|---|---|
| list, show, switch to, `config show` | ✓ | ✓ | ✓ | ✓ | ✓ |
| build, raw `cpf run`; `cmd` with `needs_build` | ✓ | – | ✓ *(judging, draft)*: the solution target only; raw `cpf run` connects the solution to the console, without an interactor | – | – |
| `cpf test` | ✓ | – | ✓ *(judging, draft)*: needs `[check] kind = "interactive"` (§9.1) | – | – |
| `cmd` with `for_each_test` | ✓ | – | – (a test input is not a dialogue) | – | – |
| *(stress, draft)* `cpf stress` (search, `--replay`, `--replay-binaries`) | ✓: needs `[stress]` (§9.2) | – | ✓: needs `[stress]`, `[check] kind = "interactive"` and `answer = "none"` | – | – |
| `check` against a recorded answer (design §5.4) | – | ✓ | – | – | – |
| problem-dependent `cmd` without build/test dependencies (e.g. `echo {problem_id}`) | ✓ | ✓ | ✓ | ✓ | ✓ |
| problem-independent `cmd` | ✓ | ✓ | ✓ | ✓ | ✓ |

Refused operations say "unsupported in this version".
*(judging, draft)* The **effective kind** is `interactive` when
`[problem] kind = "interactive"` or `[check] kind = "interactive"`, else
the `[problem] kind` (default `batch`). `[check]` is the execution-affecting
declaration and the `[problem] kind` tool field is metadata (storage
contract §3), so `[check] kind = "interactive"` on a problem whose kind is
absent or `batch` is interactive; `[problem] kind = "interactive"` with a
`[check] kind` other than `interactive` is a phase-2 error for `cpf test`
("interactive problem needs `[check] kind = \"interactive\"` and an
interactor"). A `[check]` on a problem of kind `answer`, `multi-file` or an
unknown kind changes nothing: their refused operations stay refused.
A version that does not implement a part of §9.1 rejects its keys on
presence (even with default values) for `build`, `run`, `test` and `cmd`
with `needs_build` / `for_each_test`, as a configuration planning error
before admission saying "unsupported in this version", and never judges
such a problem with the comparator instead.
A grader-linked task whose local grader is one ordinary program (§6.2 file
roles) is `batch`:
the contestant implements an interface, but execution is batch. `multi-file`
stays refused and is not reinterpreted. Unknown kind strings
are preserved in `problem.toml`, never rewritten. `check` compares the
user-recorded candidate answer with the recorded reference; no build or run
is involved.

### 9.1 Judging: `[check]` *(judging, draft)*

`[check]` and its subtables live in `problem.toml`, beside `[config]`
(§1.1). The storage record classification of `problem.toml` (storage
contract §9) does not decode them: an invalid `[check]` is a configuration
diagnostic (§3.2), never a `malformed` problem record, so the problem stays
listable and selectable. Every command that loads the selected problem's
configuration validates it (phases 1–2; `cpf config check` included);
`cpf config show` prints it as the member `check`, with defaults, in the
§10 mapping. Fetch never writes `[check]`, and no role spec, path or
argument is ever taken from fetched data (design §7).

Batch problem with a checker and a validator:

```toml
[check]
kind = "checker"                 # "compare" (default) | "checker" | "interactive"
checker = { sources = ["checker.cpp"], support_files = ["testlib.h"], language = "cpp", profile = "testlib" }
validator = { sources = ["validator.py"], language = "python", profile = "simple" }
answer = "file"                  # "file" (default) | "none"
scoring = { points = true, scale = 100, aggregation = "sum" }   # default "pass_fail"
display = "cpf"                  # "cpf" (default) | "judge-cf"
```

Interactive problem (every key of `[check.interactive]` with its default
where it has one):

```toml
[check]
kind = "interactive"

[check.interactive]              # required iff kind = "interactive"
interactor = { sources = ["interactor.cpp"], support_files = ["testlib.h"], language = "cpp", profile = "testlib", seed = "env" }
checking = "interactor"          # required: "interactor" | "checker" | "interactor+checker" (then `checker` is required)
mode = "relay"                   # "relay" (default) | "direct"
idle_s = 0                       # 0 (default): watchdog off
early_reader_close = false
interactor_allowance_s = 1.0
peer_allowance_s = 1.0
solution_seed = "none"           # "none" (default) | "env"

[check.interactor_test]          # optional; used by `cpf interactor-test`
good = [{ sources = ["solutions/ok.cpp"], language = "cpp" }]
bad = [{ sources = ["solutions/wa.cpp"], language = "cpp" }]
```

#### 9.1.1 Keys

Every key not listed is a phase-1 error naming it; `check.interactor`
(outside `[check.interactive]`) is such an unknown key.

| Key | Type | Default | Rules |
|---|---|---|---|
| `kind` | string | `"compare"` | `compare`: the built-in comparator of `compare` (§1.1, compare spec); `checker`: the `checker` judges every test; `interactive`: one session per test (checkers design §§5–6) |
| `checker` | role spec | – | required iff `kind = "checker"`, or `kind = "interactive"` with `checking` `checker` or `interactor+checker`; present otherwise → phase-2 error |
| `validator` | role spec | – | optional with every kind |
| `answer` | string | `"file"` | `"file"` or `"none"` (§9.1.4); `"none"` with kind `compare` → phase-2 error |
| `scoring` | `"pass_fail"` or table | `"pass_fail"` | table `{ points, scale, aggregation }`: `points = true` required (`false` is an error: write `"pass_fail"`), `scale` an integer 1–10⁹, `aggregation` `"sum"` (default) or `"min"`; a table with kind `compare` → phase-2 error |
| `display` | string | `"cpf"` | `"cpf"` or `"judge-cf"` (report contract §2.8); display only, never a stored result |
| `interactive` | table | – | required iff `kind = "interactive"`; present otherwise → phase-2 error |
| `interactive.interactor` | role spec | required | profile `testlib` only |
| `interactive.checking` | string | required, no default | `interactor`, `checker`, `interactor+checker` (checkers design §6.3) |
| `interactive.mode` | string | `"relay"` | `relay` or `direct` (checkers design §5.2) |
| `interactive.idle_s` | duration or `0` | `0` | the number `0` turns the watchdog off; any other value follows §7.1, so one that rounds to 0 ms is an error; non-zero with `mode = "direct"` → phase-2 error |
| `interactive.early_reader_close` | boolean | `false` | checkers design §6.5 |
| `interactive.interactor_allowance_s` | duration | 1 s | §7.1 |
| `interactive.peer_allowance_s` | duration | 1 s | §7.1 |
| `interactive.solution_seed` | string | `"none"` | `none` or `env` (§9.1.6) |
| `interactor_test` | table | – | only with `kind = "interactive"` (§9.1.7) |
| `interactor_test.good`, `.bad` | arrays of solution specs | `[]` | §9.1.7 |

#### 9.1.2 Role specs and their stages

A **role spec** is an inline table, built from source or prebuilt:

| Field | Type | Rules |
|---|---|---|
| `sources` | array of problem-relative paths | built roles: required, non-empty; the §6.2 phase-1 path rules of `sources` (duplicates by the same rule) |
| `support_files` | array of problem-relative paths | built roles; default `[]`; §6.2 rules; a path in both lists → phase-2 error |
| `language` | language name | built roles: required; a language of the effective configuration (phase 2) |
| `entry` | string | built roles, optional; the language `entry` rules (§6.4); default the language's `entry` |
| `program` | problem-relative path | prebuilt roles: required; the §6.2 path rules; exclusive with `sources`, `support_files`, `language` and `entry` |
| `profile` | string | required: checker `testlib` or `simple`; validator `testlib` or `simple`; interactor `testlib` |
| `deadline_s` | duration | optional, §7.1; not allowed on the interactor (its deadline is derived, checkers design §5.6) |
| `stdout_cap`, `stderr_cap` | size | optional, §7.1; `stdout_cap` not allowed on the interactor (its stdout is the dialogue, capped by checkers design §5.5) |
| `seed` | string | interactor only: `none` (default), `env` or `argv` (§9.1.6) |
| `assets` | array of problem-relative paths | optional, default `[]`; files the judge opens at run time by a relative path (checkers design §2.4); the §6.2 path rules; captured with the role and staged into the private directory at the same relative path (execution contract §2.1); a first component equal to `input`, `output`, `answer`, `tout`, `report` or `transcript` (compared as for duplicates) → phase-2 error; a path also in `sources`, `support_files` or `program` → phase-2 error (a file is captured once) |

Unknown fields are phase-1 errors. The role's **effective stages** are
resolved like the target's, with these differences:

- **Built role**: the build stage is the language's build stage composed
  from §6.1 rows 1–3 only (no profile row 4, no problem row 5, no CLI row
  6); `{sources}` / `{source}` expand to the role's captured sources
  (§9.1.3), `{artifact}` / `{artifact_dir}` to the role build's fresh
  output root (storage contract §6.1), `{entry}` to the role's effective
  entry. A language without a build stage gives a build-free role (no
  role build). The run stage is the language's run stage from rows 1–3,
  then the role spec's `deadline_s`, `stdout_cap` and `stderr_cap` as the
  highest row; `--deadline` never applies to roles.
- **Prebuilt role**: the run stage is `argv = [<captured program>]` with
  the run-stage defaults (execution contract §5.5) and the role spec's
  `deadline_s` and caps; the environment is the §4.3 baseline.
- **Profile arguments** (execution contract §2.1) are appended to the
  role's run `argv` (or to shell `args`, §5.3) after expansion; they are
  never expanded and never rebased.
- A composed role stage whose `cwd` is not `{ selector = "run" }` is a
  phase-2 error: the run's fresh `cwd/` is the role's **private
  directory** (execution contract §2.1). So is a role stage that uses
  `{problem_dir}`, `{workspace}`, `{test_input}`, `{test_answer}` or
  `{iteration}`; `{problem_id}` is allowed.
- The interactor's deadline is the solution run's effective deadline plus
  `interactor_allowance_s`; the session deadline is the later of the two
  plus 1 s (checkers design §5.6).
- Local limits (§7.2) of the composed role stages are validated in phase 3
  like the target's: a limit the platform cannot enforce is an error.

#### 9.1.3 Role capture

Roles are frozen with the job, like the target's captured files (storage
contract §7):

- A built role's captured files are its `sources`, then its
  `support_files`, then its `assets`; a prebuilt role's are its `program`
  then its `assets`. Each gets the §6.2 phase-3 checks (existing regular
  file, no link component, outside reserved trees, same-file aliases
  within the role).
- At freezing, after the target's captured files, each role's files are
  copied into `inputs/roles/<role>/` (`<role>` is `checker`, `validator`
  or `interactor`) with their problem-relative paths, by the storage
  contract §7 copy rules (regular files only, exclusive creation, counted
  in the job budget); a prebuilt program's copy is made executable (Unix
  mode `0700`). A collision or copy failure is a capture failure: the job
  is refused (class `workspace`, exit 8) and never ready.
- Every stage of the job uses the copies, never the originals. A role
  build is a source-consuming stage: the accident guard (storage contract
  §7) applies to it and the role's capture check (`checked` or
  `unchecked{shell_stage}`) is recorded per role (report contract §4.4).
- The capture manifest (report contract §4.2) also lists every role's
  captured files with full values.
- **Judge fingerprint**: SHA-256 over a byte string in the conventions of
  the workspace design §3.2 `source` fingerprint (little-endian `u64`,
  length-prefixed byte strings, problem-relative paths): the byte string
  `"cpf-fp-judge-v1"`; the report member `target.check` (report contract
  §4.3) in the report contract's §3.1 encoding with its `display` member
  removed (display never changes a result); then for each declared role in
  the order checker, validator, interactor: a tag byte `0` = absent or `1`
  = present, and when present the role token, the profile token, a tag
  byte `0` = built or `1` = prebuilt, and:
  - the captured files (count, each path and content, ascending path
    order), so a changed byte changes it;
  - the role spec's lists in configured order, each as count then paths:
    `sources`, `support_files`, `assets` (built), or `program` and `assets`
    (prebuilt), so moving a file between lists changes it;
  - built: the language name, the effective entry (tag byte `0` = none or
    `1` then the entry), the composed role build stage (tag byte `0` =
    build-free or `1` then its covered settings) and the composed role run
    stage, each with the covered build settings of the `source`
    fingerprint (command, `cwd`, `env_remove`, `env`, merged and validated,
    not expanded);
  - the role spec's `deadline_s`, `stdout_cap`, `stderr_cap` (tag byte
    `0` = unset or `1` then the value as `u64` microseconds or bytes) and
    `seed` token, which change a judge's outcome.

  Program resolution and toolchains are not covered, as for `source`. It is
  computed from the copied bytes during freezing and is `null` for a
  problem without a role. `cpf status`
  computes the same value over the current `[check]` and files (no tool
  is probed).

#### 9.1.4 Answers

The **answer states** of a test (checkers design §3) are three, and
`tests.toml` already distinguishes them without a new key (storage
contract §8.1):

| State | `tests.toml` | Treatment |
|---|---|---|
| deliberately none | `answer` omitted | depends on `[check] answer` (below) |
| legitimately empty | `answer` names a zero-byte file | a present answer, empty |
| absent by accident | `answer` names a file that does not exist | an unhealthy entry: the job is refused before any run (storage contract §8.1, exit 8); never an answer state |

`[check] answer` is the problem-level policy for the first state:

- `"file"` (default): with kind `checker` or `interactive`, a selected
  test whose answer is deliberately none is a phase-3 error naming the
  tests (exit 7), before anything runs. With kind `compare` the existing
  rule stays: such a test is `unchecked{no_answer}` (compare spec §1.1,
  report contract §2.4), because the comparator cannot run without an
  answer while a checker could misjudge a fabricated one.
- `"none"`: an explicit author assertion that the problem's judges accept
  an empty answer. A test whose answer is deliberately none gets an empty
  file in every judge's answer position; a test with a present answer
  gets that answer. Rejected with kind `compare` (phase 2).

#### 9.1.5 Scoring

`scoring = "pass_fail"` judges pass/fail only; a judge that reports
points is then `ProtocolUnsupported` (checkers design §4). A points table
enables scores: per-test scores, fixed-point aggregation, `incomplete`
and `no_tests` follow checkers design §4 and report contract §2.13. With
kind `interactive`, the authority that scores is `checking`'s (checkers
design §6.3): the interactor under `interactor`, the follow-up checker
otherwise.

#### 9.1.6 Interactive settings and seeds

- `mode`, `idle_s`, `early_reader_close`, `interactor_allowance_s` and
  `peer_allowance_s` configure the session (checkers design §§5.2–5.7,
  6.5). The dialogue caps (64 MiB per direction), relay queues
  (256 KiB / 4 096 records), transcript retention (1 MiB / 65 536 records
  per direction), the report cap (64 KiB) and the `tout` cap (64 MiB) are
  *fixed* (checkers design §5.5).
- **Seeds**: separate per role, drawn per session from the operating
  system's random source unless a seeded rerun supplies the recorded
  ones, and recorded in the report (report contract §4.6). Grammar: a
  decimal `u64`, ASCII digits only, no sign, no leading zero except `"0"`,
  value ≤ `u64::MAX`. Delivery per declaration:
  - `seed = "env"` (interactor) and `solution_seed = "env"`: the variable
    `CPF_SEED_INTERACTOR`, respectively `CPF_SEED_SOLUTION`, is injected
    into that role's environment only, last (§4.3 step 4);
  - `seed = "argv"` (interactor): a declaration that the interactor is a
    **wrapper** that removes a final argument before testlib registration;
    cpf appends the seed as the last argument, after the profile
    arguments;
  - `none`: no seed; the report says the session is not seed-controlled
    for that role.
- File-based I/O for the solution of an interactive problem (`[config]
  io`, §6.2) is a phase-2 error whatever the version's `io` support
  (checkers design §9). File-based I/O of a batch solution, including
  which output a checker or comparator receives, is defined with `io`
  itself (§6.2); nothing here restricts it, so the checkers design §9
  capability matrix holds as written.

#### 9.1.7 `interactor-test` declaration

`[check.interactor_test]` declares known solutions for `cpf
interactor-test` (checkers design §7): `good` and `bad` are arrays of
**solution specs** `{ sources, support_files?, language, entry? }` with the
built-role field rules of §9.1.2. The command judges each declared
solution like `cpf test` judges the target, with the problem's selected
tests and judging. It succeeds iff every `good` solution's job verdict is
`passed` (or `scored` with a full aggregate) and every `bad` solution's job
has at least one test whose verdict is a solution verdict (`wrong_answer`,
`presentation_error`, `wall_deadline`, `session_limit`, `idle`,
`output_budget`, `signal`, `nonzero_exit`) or a non-full score; a `bad`
solution that passes fails the command. Passing it does not prove the
adaptive invariant. Both lists empty → phase-2 error for `cpf
interactor-test`. The command line and its document are specified with
the other commands (`../commands.md`).

#### 9.1.8 Validation phases

| Phase | `[check]` checks |
|---|---|
| 1 raw | key set, types, string values, path rules of `sources`, `support_files`, `program`, durations and sizes (§7.1), `scale` range, field exclusivity inside a role spec |
| 2 effective | kind ↔ `checker` / `interactive` / `interactor_test` presence; `checking` ↔ `checker`; `answer = "none"` and points tables against kind `compare`; profile allowed for the role; `idle_s` with `direct`; role `language` defined, with a complete run stage; composed role stage `cwd` and placeholders (§9.1.2); `assets` names and overlaps; `io` with an interactive problem; the effective-kind rule of §9 |
| 3 selected plan (`cpf test`) | role captured files present (§9.1.3); role program resolution (§4.4); role local limits (§7.2); the answer policy against the selected tests (§9.1.4); the run count, the judged-test cap and the report reservation cap (§8.2) |
| 4 post-build | each role build's declared outputs (§6.3), after the role build's exit 0 |

On Windows a judged `cpf test` with a valid configuration is refused with
exit 10 before admission, after phases 1–2 and the early refusals and
before phase 3 (report contract §5.2).

### 9.2 Stress: `[stress]` *(stress, draft)*

`[stress]` lives in `problem.toml` beside `[check]` (§1.1) and is owned by
this contract like `[check]` (§9.1: not decoded by the storage record
classification, an invalid table is a configuration diagnostic, never a
`malformed` problem record). It is validated (phases 1–2) by `cpf stress`
in every mode, `cpf config check` and `cpf config show` (member `stress`,
with defaults, in the §10 mapping); other commands do not read it, so an
invalid `[stress]` never blocks `build`, `test`, `run` or `cmd`. Fetch
never writes it, and no part of it is taken from fetched data (design §7).
Judging (`[check]`: comparator or checker, validator, interactor, answer
policy, scoring) is reused as declared; `[stress]` adds only the stress
roles and budgets.

```toml
[stress]
generator = { sources = ["gen.cpp"], language = "cpp" }        # required
brute = { sources = ["brute.cpp"], language = "cpp" }          # see the brute rule below
iterations = 10000                                             # default iteration budget
time_s = 60                                                    # default time budget
# min_score = "90"                                             # points problems only; default the scale
```

| Key | Type | Default | Rules |
|---|---|---|---|
| `generator` | stress role spec | required | invoked as `gen <seed>` (stress contract §3.2) |
| `brute` | stress role spec | – | required for an interactive problem, and for a batch problem unless `[check] kind = "checker"` with `answer = "none"` (a constructive problem whose checker decides validity alone); else a missing brute is a phase-2 error |
| `iterations` | integer | 10 000 | 1 ≤ value ≤ 9 223 372 036 854 775 807 (a TOML integer); `--iterations` overrides it with any `u64` ≥ 1 |
| `time_s` | duration (§7.1) | 60 s | 1 ms–24 h; `--time` overrides it |
| `min_score` | string | the scale | only with a points `scoring` (§9.1.5), else a phase-2 error; the checkers design §4 points grammar; 0 < value ≤ scale (phase 2); a scored iteration below it is a candidate failure (stress contract §4) |

Unknown keys are phase-1 errors naming them.

**Stress role specs** (`generator`, `brute`): the role spec of §9.1.2
with the fields `sources`, `support_files`, `language`, `entry`,
`program`, `deadline_s`, `stdout_cap`, `stderr_cap` and `assets`, by the
same rules (built or prebuilt, field exclusivity, path rules, asset
names); `profile` and `seed` are unknown fields for these roles (phase 1).
Their stages are composed by §9.1.2 (built role: language rows 1–3, then
the role spec's `deadline_s` and caps; prebuilt role: `argv = [<captured
program>]`; the `cwd` must be `{ selector = "run" }` and the forbidden
placeholders are the same; `{iteration}` is not available). The
generator's seed is appended after expansion as its last argument (stress
contract §3.2). In an interactive problem the brute's run stage is the
session's solution stage (its stdin and stdout are connected), as the
target's is. Role capture is §9.1.3 with `<role>` `generator` and `brute`
(`inputs/roles/generator/`, `inputs/roles/brute/`), after the judges'
roles; they are not covered by the judge fingerprint.

**Validation phases** for `cpf stress`:

| Phase | `[stress]` checks |
|---|---|
| 1 raw | key set, types, role spec fields and their path rules, `iterations` range, `time_s` (§7.1), the `min_score` grammar |
| 2 effective | the effective kind (§9) `batch` or `interactive`, else "unsupported in this version"; the brute rule; an interactive problem with `[check] answer` other than `"none"` ("interactive stress needs `answer = \"none\"`: a generator writes no answer"); `min_score` against `scoring` and the scale; `[config] io` declared → "stress with file-based I/O is unsupported in this version"; role `language` defined with a complete run stage; composed stage `cwd` and placeholders; the `[check]` checks of §9.1.8 phase 2 |
| 3 selected plan | captured files present (§9.1.3); program resolution (§4.4); local limits (§7.2); the CLI budgets and `start + iterations − 1 ≤ u64::MAX` (stress contract §2.1); for `--replay` / `--replay-binaries` the failure checks of stress contract §8 |
| 4 post-build | each role build's declared outputs (§6.3) |

On Windows `cpf stress` with a valid configuration is refused with exit 10
after phases 1–2 and the early refusals, before phase 3 (stress contract
§2.5).

## 10. `cpf config`

- `cpf config show [--origin] [--json] [--profile p] [--problem id]`: the
  effective **declarative** configuration with derived defaults; values known
  only inside a job (fresh artifact paths, test files) are shown as
  deferred. Outside a workspace: layers 0–1 only.
- `cpf config check [--tools]`: phases 1–2 for every layer, language,
  profile and command, side-effect free. `--tools` probes the external
  programs of every language, profile and command for the current context
  (current problem if any); generated programs are reported "deferred
  (generated)", and any language, profile or command whose programs depend on
  an unavailable context (no current problem, a placeholder that needs a job)
  "not checkable here" (neither is an error). Exit 0 or 7 (report contract
  §5.5).
- `cpf config paths`: the file per layer.
- JSON follows report contract §3 (canonical profile, reader rules).
  Mapping of configuration values: TOML string → JSON string; integer →
  decimal string (`i64`); float → `dec` (`{"text", "bits"}`), for every
  float, not only comparator fields; boolean → JSON boolean; array → JSON
  array; table → JSON object in key order of the effective configuration;
  native paths and values (program paths, environment values, file paths)
  → `ns` (`{"utf8"}` iff valid UTF-8 without control characters, else
  `{"bytes_b64"}`; Windows WTF-8). TOML date-times are not accepted by any
  configuration key (phase-1 error), so they need no mapping. The document
  has `schema`, `document: "config_show"` and `platform`. Environment: only keys set or
  removed by configuration are shown; the inherited environment is never
  dumped.
- No `set` / `edit` in the CLI foundation.

## 11. Conformance fixtures

Each fixture asserts exact effective values, provenance and the resolved spec.

- Merge: `argv` ↔ `shell` switch; `file` ↔ `dir` artifacts; `build = false`;
  `env = { X = false }`; `limits = {}`; overlay keys vs a command named
  `linux`; nesting refused; a later generic value over an earlier platform
  value.
- Reserved file-I/O: valid problem `io` with input only, output only or both
  → phase-2 diagnostic at `config.io`, exit 7 for build/run/test and read-only
  inspection; shared `cwd` → the same rejection; no job directory and unchanged
  job-seq. Separators, `..`, `:`, device names, trailing dots/spaces, wrong types,
  unknown nested keys or an empty table → phase-1 diagnostics with provenance,
  without a derived unsupported-I/O error. User/workspace `io` → unknown key.
  Windows: configuration errors → 7 before the platform refusal; otherwise
  job commands → 10 before admission (report contract §5).
- Validation: a stage completed across layers; a wrong type in an overridden
  layer still reported; an inactive Windows limit on Linux accepted.
- *(organisation, draft)* `status.values`: user `["wa"]` alone → effective
  `["wa"]`; user `["wa"]`, workspace `["x"]` → `["x"]`; workspace `[]` →
  `[]`; `["solved"]`, `["wa", "wa"]`, `["Wa"]`, `["a-"]`, `[1]`, 33 elements,
  `status = { values = [], other = 1 }`, and `status.values` in a
  `problem.toml` `[config]` → phase-1 diagnostics with provenance (7 from
  `cpf config check`).
- **Cases with exact results:**
  - C++ built-in build: `{artifact}` expands to `<output root>/main`
    (`main.exe` on Windows); a build exiting 0 without writing it →
    `BuildFailed{missing_output: "main"}`.
  - User `env = { X = "1" }`, workspace `env = { X = false }`, baseline
    `X=0` → final env has no `X`; adding problem `env = { X = "2" }` → `X=2`.
  - User `cwd = { path = "a" }`, workspace `cwd = { selector = "problem" }` →
    effective `{ selector = "problem" }`, no `path`.
  - Generic `argv` + `windows.shell` in one layer → valid; both in the
    generic table → phase-1 error.
  - `argv = ["./bin/x"]` in `/u/cfg.toml` → `/u/bin/x`; `argv =
    ["{problem_dir}/x"]` → not rebased, expanded to `<problem dir>/x`;
    `argv = [""]` → error.
  - Resolver: `PATH` unset + `argv = ["g++"]` → not found; Windows
    `PATHEXT` unset + `tool` → `.COM`, `.EXE`, `.BAT`, `.CMD` tried in order;
    `TOOL.Exe` tried as is only; `C:\t\tool` → as is, then extensions;
    `x.ps1` → tried as is only (no `PATHEXT` appended), skipped as not
    launchable; if nothing else matches, the error lists it.
  - `argv = ["./{problem_id}/tool"]` → load error (placeholder not at the
    start); `argv = ["{workspace}/{problem_id}/tool"]` → valid, not rebased.
- **v3 schema cases, exact results:**
  - `argv = ["g++", …]` in `/u/cfg.toml` → not rebased, `PATH` lookup;
    `argv = ["bin/x"]` → `/u/bin/x`; `cwd = { path = "out" }` → `/u/out`;
    `argv = ["/opt/{problem_id}/tool"]` → valid, not rebased.
  - Built-in Python (`build = false`), user `[languages.python.build] argv
    = ["py-compile", "{sources}"]` → build stage with that `argv`, the
    applicable defaults and modifiers, and no discarded fields;
    workspace `[languages.python] build = false` on top → build-free again,
    and a workspace `[build] deadline_s = 5` is reported as ignored.
  - User `[languages.cpp.profiles.fast.run] deadline_s = 3`, problem
    `[config.run] deadline_s = 7`, `--profile fast` → 7 (row 5 over row 4).
  - `[config] commands = …` in `problem.toml` → phase-1 error (layer);
    `[languages.cpp] extension = […]` → unknown key; `env = { CPF_X = "1" }`
    → error; `hooks.enabled` in a workspace or a `--config` file → error.
  - A hook command with `repeat = 1` → phase-2 error.
  - `shell = { interpreter = ["{workspace}/sh", "-c"], script = "true" }`
    → phase-1 error (no placeholders in the interpreter).
  - Selected problem without `language`, no `default_language`: `cpf config
    check` → exit 0, target unresolved; `cpf test` → error.
  - Unreadable user file and a workspace `on_switch = ["notify"]` with no
    `notify` command → only the unreadable-file error (the reference could
    be defined in the user layer).
  - Global `[build] argv = ["cc"]`, `[languages.cpp] build = false`,
    `--profile p` with `[languages.cpp.profiles.p.build] env = { A = "1" }`
    → a complete build using `cc` and `A=1`.
  - A complete `shell` in the user layer, workspace `shell = { args = ["x"]
    }` → phase-2 errors: `interpreter` and `script` missing.
  - Same layer: `[languages.cpp.run.windows] argv = ["a"]` and
    `[languages.cpp.windows.run] argv = ["b"]` → `["b"]` on Windows, the
    generic run elsewhere.
  - User `[languages.cpp.build] env = { A = "1" }`, workspace
    `[languages.cpp] build = false`, workspace
    `[languages.cpp.profiles.p.build] argv = ["c++", "{sources}", "-o",
    "{artifact}"]`, global `[build] deadline_s = 9`, `--profile p` → build
    with that `argv`, no `A`, deadline 9 s; without `--profile` →
    build-free, `[build]` ignored.
  - A syntax error in the user file and an unknown key in the workspace
    file → both reported; the workspace's other keys still checked.
  - `cpf add` with `languages.cpp.template = { path = "t.cpp" }` and problem `sources = ["a.cpp",
    "b.cpp"]` → error, nothing published; `cpf config check` → exit 0.
- *(draft)* **Ignore patterns (§7.4), exact results:**
  - `*.o` matches `a.o` and `x/y/a.o`; `x/*.o` matches `x/a.o`, not
    `x/y/a.o` or `y/x/a.o`; `**` matches every file at any depth;
    `x/**/a.o` matches `x/a.o` and `x/p/q/a.o`; `a[b]`, `!a`, `a\b`,
    `""`, `/a`, `a/../b` → phase-1 error.
  - `?.txt` matches `é.txt` (one scalar value, two UTF-8 bytes); `??.txt`
    does not.
  - Linux component bytes `61 FF 2E 6F` (`a`, invalid byte, `.o`): `a?.o` and
    `*.o` match; the pattern `aÿ.o` (U+00FF written as UTF-8) does not.
  - Windows name UTF-16 `0061 D800 002E 006F` (lone high surrogate): `a?.o`
    and `*.o` match; `a??.o` does not; no literal pattern text matches the
    surrogate unit.
  - Windows and macOS: `*.O` matches `a.o`, `É.txt` does not match `é.txt`
    (ASCII-only folding); Linux: `*.O` does not match `a.o`.
  - Composed vs decomposed `é` (U+00E9 vs `e` U+0301): no match on any
    platform (no normalisation); `?.txt` matches only the composed form.
- *(draft)* **Source capture accident guard (storage contract §7)**, workspace
  `/abs/ws`, job directory `/abs/ws/.cpforge/runs/J`:
  - `argv = ["g++", "{sources}", "-o", "{artifact}"]` → valid (expansions
    are inside the job directory), capture check `checked`.
  - `argv = ["g++", "{problem_dir}/a.cpp"]` → phase-3 error (rule 1).
  - `"-I/abs/ws/lib"` → error (operand `/abs/ws/lib` after the first two
    characters); `"--include-dir=/abs/ws/lib"` → error (text after `=`);
    `"-I../../../lib"` from `cwd` `/abs/ws/.cpforge/runs/J/cwd` → error
    (resolves to `/abs/ws/.cpforge/lib`, outside the job directory).
  - `"/backup/abs/ws/a.cpp"` → valid (not inside `/abs/ws`);
    `"/abs/ws/../ext/a.cpp"` → valid (normalises to `/abs/ext/a.cpp`);
    `"/abs/wsx/a.cpp"` → valid; `"-I/usr/include"` → valid, external.
  - `"-Wl,-rpath,/abs/ws/lib"` → valid: not split (rule 3), and the whole
    element, a relative path, resolves inside the job directory; the
    workspace path inside it is not detected.
  - From `cwd` `/abs/ws/.cpforge/runs/J/cwd`: `"-Wl,-rpath,../../../../lib"`
    → error (whole element resolves to `/abs/ws/.cpforge/runs/lib`);
    `"-I../../../../../ext"` → error (whole element resolves to
    `/abs/ws/.cpforge/ext`, although `-I`'s operand resolves to `/abs/ext`):
    conservative by rule 2.
  - Windows, workspace `C:\Ws`: `"/IC:\ws\lib"` → error (operand
    `C:\ws\lib` after `/I`, inside the workspace under ASCII case folding).
  - A shell build stage with `args = ["{sources}"]` and `cwd` selector
    `run` → valid, capture check `unchecked{shell_stage}`; the same stage
    with `cwd` selector `workspace` → phase-3 error (rule 1).
  - `sources = ["e\u0301.cpp", "\u00E9.cpp"]` on macOS APFS (one file,
    two spellings) → phase-3 error (same file); `["a.cpp", "A.cpp"]` →
    phase-1 duplicate on Windows and macOS; on Linux two files → valid.
  - Python (build-free): run stage `argv = ["python3", "{source}"]` →
    `{source}` names `inputs/src/<file>`; capture check `checked`.
  - A raw `[languages.cpp.windows.run]` table with both `argv` and `shell`
    → phase-1 error even though no other table defines either.
  - `CwdOverflow` on one run of `cpf cmd` → recorded, exit 0 if all runs
    otherwise succeeded; `BuildOutputTooLarge` → build failure, no run.
  - Numbers: `deadline_s = 0.0005` → 1 ms; `0.0004` → error (below 1 ms
    after rounding); `"1.5MiB"` → error; `repeat = 0` → error.
  - `cmd` with `cwd.selector = "problem"` and no placeholders outside a
    problem → plan error (context required); `echo {problem_id}` for an
    `interactive` problem → runs.
  - Two runs, one `OutputBudget` → exit 1 (report contract §5.2).
  - Trust: listed `/example/cp` and root `/example/cp/` via a symlink
    `/example/link` → trusted after canonicalisation; listed path missing →
    untrusted + warning; Windows `C:\CP` vs `c:\cp` → trusted.
  - `after_test` after failing tests → runs; after Ctrl-C → does not; hook
    command with `deadline_s = 5` → 5 s, without → 30 s.
  - `config check --tools` with a command using `{artifact}` → "deferred
    (generated)"; a `for_each_test` command outside a problem → "not
    checkable here"; exit 0.
- Precedence rows 1–6, including a user profile over a workspace language.
- Paths: config-relative template after a compiler-only override;
  problem-relative sources; `~/`; drive-relative refused; non-UTF-8 names.
- Env: remove then set; case-insensitive override and same-map duplicate on
  Windows; NUL refused; `CPF_*` not overridable.
- Resolver: empty, missing and relative `PATH` entries; `PATHEXT` order;
  explicit extension; directory or non-executable candidate skipped; not
  found.
- Generated program checked only after its build; batch argument refused
  (`UnsafeBatchArgument`) and a batch name with trailing dot or space on
  Windows CI.
- Grader amendment: C++ solution + grader + header builds with the
  built-in command and the header is captured but not passed to `g++`;
  Python grader in `sources` imports the captured `solution.py` from a fresh
  cwd and creates no `__pycache__`; Java `entry = "judge.Grader"` derives
  `judge/Grader.class`; a path in both `sources` and `support_files` →
  phase-2 error; a `solution_files` member outside the captured files →
  phase-2 error; a missing support file → phase-3 error and nothing runs;
  a materialisation whose prospective record has
  `solution_files = ["solution.cpp"]` and two sources writes the template
  to `solution.cpp`; empty or two-member `solution_files` with a template →
  fails before publication; moving a file between `sources` and
  `support_files`, or changing `config.entry`, changes the `source`
  fingerprint without a byte change; `config.entry = "/tmp/Grader"` for
  Java → phase-2 error; a Java sibling only in `support_files` is not
  compiled (documented limitation, not a fixture of success); single-file
  targets unchanged.
- Expansion amendment: Windows environment names differing only outside
  ASCII case (e.g. `Ä`/`ä`) are one name; `PATH` set in a stage's `env` is
  the one searched; `tool.` and `tool.cmd ` refused on Windows; malformed
  `PATHEXT` entries skipped; a Unix file executable only by another user is
  not launchable; a generated batch program has launch kind `batch`; a
  generated path with `..` refused; a placeholder value supplied for an
  undeclared placeholder is still unavailable; empty `sources` expands
  `{sources}` to nothing and refuses `{source}`; an interpreter symlink
  named `sh` is accepted; an interpreter under the artifact directory
  refused; with `tool.cmd` and `tool.cmd.exe` both present the launcher
  starts `tool.cmd` (B4a).
- Placeholders: multi in program position refused; missing answer;
  `{artifact}` for a directory target; shell positional args with spaces,
  quotes, backslashes, `$` and newlines; CLI extra args verbatim.
- Artifacts: a no-op `javac` fails on `required`; packaged Java with inner and
  auxiliary classes; Python build-free; `..` in artifact names refused.
- Deadlines: unset vs default; published-limit derivation; `--deadline`
  scope; NaN and overflow refused; cap boundaries.
- Hooks: untrusted workspace; `--config` disables; `enabled` in a workspace is
  an error; hook-context validation; a concurrent switch sees the captured
  context; depth guard.
- Repeat: ordering, zero tests, stop on failure, `keep_going`, cancellation;
  9 999 / 10 000 / 10 001 planned runs.
- `config show` / `check`: no side effects, stable JSON, provenance, deferred
  values, unsupported kinds.
- *(judging, draft)* **`[check]`** (each row: exact diagnostics with
  key path and provenance, exit status, and no job directory or sequence
  allocation for a refusal):
  - Kinds and roles: kind `checker` without `checker`, kind `compare` with
    `checker`, `checking = "interactor"` with `checker`, `checking =
    "checker"` / `"interactor+checker"` without `checker`, kind
    `interactive` without `[check.interactive]`, `[check.interactive]`
    with another kind, `check.interactor` outside `[check.interactive]`,
    `interactor_test` with another kind → each its phase-1 or phase-2
    error, exit 7.
  - Role specs: `sources` with `program`; `program` with `language`;
    `language` missing or undefined; a path with `..`, `\` (Windows), an
    empty component or a duplicate; a path in both `sources` and
    `support_files`; profile `simple` for the interactor; profile
    `testlib-validator` (unknown value); `deadline_s` or `stdout_cap` on
    the interactor; `seed` on a checker; a language run stage with `cwd =
    { selector = "problem" }`; a role stage using `{workspace}` → errors;
    `{problem_id}` in a role stage → valid.
  - Answers: kind `checker`, answer `file`, one selected test without an
    answer → phase-3 error naming the test, exit 7, nothing runs; the same
    test unselected (`--case`) → valid; `answer = "none"` → the judge's
    answer file is empty for that test and the present answer for another;
    `answer = "none"` with kind `compare` → phase-2 error; kind `compare`
    with an answerless test → `unchecked{no_answer}` (unchanged); an entry
    naming a missing answer file → exit 8.
  - Scoring: `scale` 0, 1, 10⁹, 10⁹ + 1, `1.5`, `"100"`; `points = false`;
    unknown `aggregation`; a points table with kind `compare` → errors
    except 1 and 10⁹.
  - Interactive: `idle_s = 0` with `direct` → valid; `idle_s = 1` with
    `direct` → phase-2 error; `idle_s = -1` and `"0.0004s"` → phase-1
    errors; defaults of every key shown by `config show`; `[config] io`
    on an interactive problem → phase-2 error.
  - Effective kind: `[problem] kind = "interactive"` without `[check]` →
    `cpf test` phase-2 error, `cpf build` and raw `cpf run` valid; `[check]
    kind = "interactive"` on a problem without `[problem] kind` → an
    interactive `cpf test`; `cmd` with `for_each_test` on an interactive
    problem → refused, exit 7.
  - Run counts: one test with validator, checker and a C++ checker and
    validator → 3 builds + 3 runs; an interactive test with checking
    `interactor+checker` and a validator → 4 runs per test; 1 000 judged
    tests → valid, 1 001 → phase-3 error; a compare problem with 9 999
    tests and one build → valid (unchanged cap); a judged plan whose
    report reservation exceeds 256 MiB (many hooks) → phase-3 error.
  - Capture: a role source that is a link, a FIFO or missing → phase-3
    error; two role paths naming one file → phase-3 error; a prebuilt
    program copy is executable and run from `inputs/roles/`; an edit of a
    role source after freezing does not change the running job.
  - Assets: `assets = ["data/words.txt"]` → staged as
    `<private dir>/data/words.txt` and opened by a relative path from the
    judge; an asset named `report` or `input/x` → phase-2 error; an asset
    also in `support_files` → phase-2 error.
  - Judge fingerprint vectors: the same `[check]` and files give the same
    digest on every platform; a changed checker byte, profile or `scale`
    changes it, `display` does not; moving a file from `sources` to
    `support_files` with the same concatenated bytes, changing the role's
    `entry`, its language's run-stage argv, its `deadline_s` or `seed`
    each change it; reordering `support_files` changes it; a problem
    without roles has `null`.
  - Windows: a valid judged `cpf test` → exit 10 before admission; a
    `[check]` phase-1 error → exit 7 first.
