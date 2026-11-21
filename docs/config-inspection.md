# Read-only configuration inspection

```
cpf [--config file] [--problem ref] config show [--origin] [--json] [--profile p]
cpf [--config file] [--problem ref] config check [--tools]
cpf [--config file] [--problem ref] config paths
cpf [--config file] [--problem ref] doctor
```

These commands load configuration without acquiring a workspace lock, recovering
leftovers, creating job directories, reading source files or running hooks.
Outside a workspace, only built-in and user configuration contribute. A missing
selection or language leaves the target unresolved. `--problem` uses the ordinary
read-only selection resolver and takes precedence over the current directory.

`show` prints effective language stages, each profile, commands, defaults and,
when resolvable, the selected problem under `target`. `--origin` adds supplying
locations, overlays and ordered provenance events, including deleted and ignored
values. Placeholders and fresh working directories remain marked deferred.
Only configured environment entries and removals appear, including tombstones;
the process's inherited environment is never included.

`check` validates phases 1–2 across all loaded layers and unused definitions.
Errors and warnings, including trust canonicalisation warnings, go to stderr.
Configuration errors select status 7; warnings select 0. With `--tools`, stdout
also lists each language/profile stage and command's program. Lookup uses the
same native resolver, launchability probe and effective environment edits as
execution, without executing a program. Results are `found <path>`, `not found`,
`deferred (generated)` or `not checkable here`. Missing external tools are
informational and do not select status 7: `check` validates phases 1–2, while
tool resolution belongs to phase 3 (configuration contract §3, §10). A compiler remains
checkable when only its arguments require captured sources.

`paths` lists the built-in layer and discovered files, with existence information.
An undiscoverable layer is described as unavailable; missing configuration,
invalid configuration or selection does not make this inventory fail.

`doctor` adds compiled supervisor support, informative runtime capability probes,
measurement and limit methods, cleanup verification, the collision-key Unicode
version, and deferred features. It launches no child as a supervisor probe and
states this explicitly. Unsupported capabilities never affect its exit status.
Each target reports its source-consuming stage's predicted capture check,
labelled plan-time: `checked` for argv and `unchecked{shell_stage}` for shell.
Operand expansion and the accident guard remain deferred until phase 3.

## Machine output

Among these commands only `config show` has a schema-1 machine document
(report contract §6.1). `doctor --json`, `config check --json` and
`config paths --json` are usage errors (status 2).

`config show --json` starts with
`{"schema":1,"document":"config_show","platform":...}`. It uses the shared
canonical writer and stdout delivery, followed by exactly one newline. Members
follow this order:

| Member | Meaning |
| --- | --- |
| `schema`, `document`, `platform` | Report document header (`unix` or `windows`). |
| `config` | Effective configuration, including composed profiles and optional `target`. |
| `origins` | Records of `path` (literal key-segment array), `origin`, optional `overlay`. Array elements have their own records. |
| `deferred` | Records of `path`, `placeholder`, `origin`; `run_cwd` denotes the fresh working directory. |
| `history` | Records of `path`, `event`, `origin`, `values`, `removed`; snapshots contain `origin` and `value`. |
| `capture_checks` | Records of `target`, `stage`, `check` (the job report's form: `{"kind":"checked"}` or `{"kind":"unchecked","reasons":["shell_stage"]}`), `scope: "plan_time"`, `guard: "deferred_until_phase_3"`. |
| `hooks_disabled_by_config_override` | Whether `--config` disables hooks for this invocation. |

Provenance uses `kind` (`file`, `built_in`, `cli`, `environment`, `derived`),
with native `path` and optional u32 `line`/`col` for files, `flag` for CLI,
`name` for environment, and recursively encoded `inputs` for derived values.
Metadata is additive within schema 1, as report §3.6 permits. This choice keeps
configuration values separate from provenance without changing their mapping.

TOML integers become decimal strings; every float becomes `dec` (`text`, `bits`);
booleans remain booleans; arrays and tables retain their structure. Native
programs, paths and environment values use `ns`, including base64 when their
bytes contain controls or cannot be represented as UTF-8. Placeholder expressions
remain declarative and are identified by the separate deferred records.

Invalid configuration and discovery or selection failures in machine mode use
the existing `error` document with class `config`, exit 7 and bounded diagnostics.
A write or flush failure selects status 11 through the shared delivery path.
