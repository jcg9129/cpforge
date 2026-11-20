# Configuration and commands contract

Status: v3 (v2 plus the schema amendment: key matrix §1.1, discovery §1.2,
precedence-preserving merge §2.1, diagnostics §3.2, program-path rules §4.4,
hook switch §8.3).

**Managed-workspace amendment (v4): settled, not yet implemented.**
`[workspace] ignore` (§7.4) specifies the design in
`../design-workspace.md` §2.4. Amended content is marked *(draft)* below and
is settled but not yet implemented. References labelled *workspace design* point to
`../design-workspace.md`.

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
| `hooks.on_switch`, `hooks.after_test` | array of command names | 1, 2 | §8.3 |
| `hooks.trusted_workspaces`, `hooks.enabled` | array of absolute paths; boolean | default user file only | §8.3; an error in a `--config` file |
| `config.language` | string | 3 | §6.2 |
| `config.sources` | array of paths | 3 | §6.2 |
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
- Problem layer: the selected problem's `problem.toml` (storage contract §9
  classification); a problem without a `[config]` table contributes an empty
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

Built once per run, in this order:

1. **Baseline:** `cpf`'s own environment, captured once at job start.
2. Remove every name in the effective `env_remove` (case-insensitive on
   Windows).
3. Apply the effective `env` map: set each name; a `false` value (tombstone,
   §2) unsets the name, so it is absent from the final environment even if
   the baseline had it. Across layers later wins, compared
   case-insensitively on Windows; two keys of **one** map differing only in
   case on Windows are an error.
4. Inject `CPF_*` variables (§8.3) last; configuration cannot override them.

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
| Unix launchable | regular file with execute permission for the effective user |
| candidate that exists but is not launchable (directory, no permission, other extension such as `.ps1`) | skipped, search continues; if no launchable match is found, the error names the skipped candidates and why |
| several matches | first launchable match wins |
| nothing found | phase-3 error; no fallback, Rust's search never used |

The absolute result goes into the spec and the record.
- `.bat`/`.cmd`: launched through Rust's batch escaping (the Rust 1.81 fixes
  are within MSRV 1.85). An argument it refuses is the launch failure
  `UnsafeBatchArgument`, never passed unescaped. Batch execution runs
  `cmd.exe` internally: the one documented exception to "no implicit shell".

### 4.5 Staged materialisation

Plans are symbolic until each launch. Phase 3 resolves external programs and
allocates output paths. A **generated** program is any expanded program path
under the run's artifact directory (`{artifact}`, or a path built from
`{artifact_dir}`); it is checked (exists, regular file, launchable per §4.4)
after its producer succeeded (phase 4) and before its consumer launches.
Every other program is resolved in phase 3, before anything runs. A fully
resolved `ResolvedExecutionSpec` exists at each launch boundary (execution contract §3.4).
Preflight does not guarantee that files stay unchanged until spawn.

## 5. Placeholders

### 5.1 Grammar and fields

- Recognised only as `{name}` with a name from §5.2; `{{` and `}}` are literal
  braces; a lone brace is an error.
- Expanded in `argv` elements, shell `args` and `cwd.path`; **never** in env
  values, the shell interpreter or script, or CLI extra args (appended
  verbatim, §8.1).
- Program position (`argv[0]`) must expand to exactly one scalar; multi
  placeholders there are an error.
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
| `{entry}` | scalar | the language declares `entry` |
| `{problem_dir}`, `{problem_id}` | scalar | a problem is selected (all stages, hooks) |
| `{workspace}` | scalar | inside a workspace (all stages, hooks) |
| `{test_input}` | scalar | command with `for_each_test` |
| `{test_answer}` | scalar | command with `for_each_test` and the test has an answer (else plan error for that test) |
| `{iteration}` | scalar, 1-based | command with `repeat` |

In **build**, `{artifact}` and `{artifact_dir}` are the allocated output paths
the build writes; **path availability ≠ output existence**: outputs are checked
only in phase 4.

Unavailable → phase-3 error naming the stage and placeholder.

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
  resolved name).
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
- *(draft)* In a managed workspace, sources are **captured** into the job
  (storage contract §7) and every `{sources}` / `{source}` expansion names
  the copy; outside the amendment they are read in place. Test data is
  always frozen.
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
argv = ["python3", "{source}"]
[languages.python.run.windows]
argv = ["python", "{source}"]

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

For Java, `required` defaults to the class file derived from `entry` (dots →
path separators, plus `.class`: `a/b/Main.class`). All other classes the
compiler writes (inner, auxiliary, other packages) stay in the output root,
which is the class path.

### 6.5 Profiles and templates

`[languages.<l>.profiles.<name>]` with optional `build` and `run`, selected by
`--profile`; unknown → phase-2 error; the record names it. `template = { path
= … }` per language (config-relative), used by `cpf add` without overwriting
(storage contract §5.2). The template is copied to the target's single
source path: `cpf add` with a template requires exactly one effective
source, and zero or several are an error before anything is published (a
mapping of several template files is a later design). Checking
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
  once first (outside the test × repeat product); only then are the artifact
  placeholders available; using them without it is a phase-2 error.
- `for_each_test = true`: one run per enabled test; stdin defaults to the
  test's frozen input (storage contract §7), `stdin = "null"` overrides. `stdin = "test"`
  without `for_each_test` is an error.
- **Context requirements** are derived from **all** fields: a selected
  problem is needed iff any placeholder of §5.2 needs one, `cwd.selector =
  "problem"`, `needs_build` or `for_each_test`; a workspace iff any of those,
  `{workspace}` or `cwd.selector = "workspace"`. Otherwise the command runs
  anywhere.
- **Exit status of `cpf cmd`:** 0 iff every planned run succeeded; any
  failure (§8.2) → the command-failure code; cancellation → the cancellation
  code; plan errors → the config-error code (codes: report transport).

### 8.2 Repeat and run counts

- `repeat = N`, 1 ≤ N ≤ 1000. Test-major order: test 1 iterations 1…N, then
  test 2, … Each iteration is a separate run with a fresh `cwd/`.
- **Failure** = non-zero exit, signal, deadline, output cap (`OutputBudget`),
  local-limit outcome, `NotStarted`, or any infrastructure outcome (execution contract §6.4).
  The first failure stops the whole command (remaining runs recorded
  `skipped`) unless `keep_going = true`. Cancellation stops before the next
  run (execution contract §6.4).
- `for_each_test` with zero enabled tests is a phase-3 error.
- Aggregates are **per test**: min, median (even count: lower middle) and max
  wall time over successful iterations.
- Planned runs = builds + tests × repeats + hooks, computed with checked
  arithmetic in phase 3; more than **10 000** is a plan error (execution contract §6.2
  report bound).

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
- `on_switch` runs after a successful switch; `after_test` once per
  `cpf test` invocation after its job finished, also when tests failed, never
  after cancellation or an infrastructure failure of the job. The hook
  plan is captured with the triggering event.
- Hooks run after the triggering command released the workspace lock,
  through the supervisor, in the listed order. Effective deadline: the
  referenced command's own `deadline_s` if set, else the hook default (30 s,
  §7.1); other settings as for that command in hook context. A hook failure is a warning in the triggering report.
  Cancelling the triggering command cancels hooks not yet started.
- `CPF_HOOK_DEPTH=1` is injected last (§4.3); a `cpf` started with depth ≥ 1
  or a malformed value runs no hooks. This prevents accidental recursion;
  hook code can remove the variable, so it is not a guarantee.
- Read-only commands (`ls`, `current`, `config`, `doctor`, `status`) never
  run hooks.

## 9. Problem kinds

| Operation | batch | answer | interactive | multi-file | unknown |
|---|---|---|---|---|---|
| list, show, switch to, `config show` | ✓ | ✓ | ✓ | ✓ | ✓ |
| build, run, test; `cmd` with `needs_build` / `for_each_test` | ✓ | – | initial feature release | – | – |
| `check` against a recorded answer (design §5.4) | – | ✓ | – | – | – |
| problem-dependent `cmd` without build/test dependencies (e.g. `echo {problem_id}`) | ✓ | ✓ | ✓ | ✓ | ✓ |
| problem-independent `cmd` | ✓ | ✓ | ✓ | ✓ | ✓ |

Refused operations say "unsupported in this version". Unknown kind strings
are preserved in `problem.toml`, never rewritten. `check` compares the
user-recorded candidate answer with the recorded reference; no build or run
is involved.

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
  "not checkable here" (neither is an error). Exit 0 or the config-error code (report transport).
- `cpf config paths`: the file per layer.
- JSON: native strings as `{ "utf8": "…" }` when valid UTF-8, else
  `{ "bytes_b64": "…" }` (Windows: WTF-8 bytes). Environment: only keys set or
  removed by configuration are shown; the inherited environment is never
  dumped.
- No `set` / `edit` in the CLI foundation.

## 11. Conformance fixtures

Each fixture asserts exact effective values, provenance and the resolved spec.

- Merge: `argv` ↔ `shell` switch; `file` ↔ `dir` artifacts; `build = false`;
  `env = { X = false }`; `limits = {}`; overlay keys vs a command named
  `linux`; nesting refused; a later generic value over an earlier platform
  value.
- Validation: a stage completed across layers; a wrong type in an overridden
  layer still reported; an inactive Windows limit on Linux accepted.
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
  - Two runs, one `OutputBudget` → exit = command-failure code.
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
