# Archive contract

Status: v1, draft. Normative for the `archive` module: `cpf archive` (plan,
snapshot, retirement, cleanup), the archive roots and their snapshots,
`cpf quarantine`, `cpf restore`, and the archive side of `cpf ls --archived`
and `cpf search`. It refines design §7a "Archive" and §11 (cleanup modes);
§11.4 lists where it narrows or changes them.

Related: design §7a, §11; `../design-workspace.md` §2; `storage.md` §1, §2,
§2.1, §3, §4, §5.6, §5.7, §5.8.10, §5.9, §6.2, §9, §10; `config.md` §1.1,
§6.2, §10; `report.md` §3, §5, §6.1. *S*, *C*, *R* and *workspace design*
point to `storage.md`, `config.md`, `report.md` and
`../design-workspace.md`.

MUST / MUST NOT / never are requirements. Numbers are *fixed* (not
configurable) unless this contract names a configuration key.

## 1. Scope and guarantees

- **Commands.** `cpf archive <target>` (plan by default, `--apply` to act),
  `cpf archive --resume`, `cpf archive --close <txn>`, `cpf restore`,
  `cpf quarantine ls|purge`, and archive roots in `cpf ls --archived` and
  `cpf search`.
- **File-only.** None of these commands starts a process, runs a hook,
  takes a job lease or uses the network. The macOS trash backend is the
  in-process `NSFileManager` call, never the Finder script (§6.5). They run
  in full on Linux, macOS and Windows, and every test of §10 runs on all
  three unless it names an OS.
- **Guarantee levels** are those of S §1, with the same threat model and
  the same stable-ancestor assumption, extended to the archive root and the
  directories below it. Level 2 (a `cpf` process killed at any point) is
  guaranteed by the crash tables of this contract and tested by killing the
  real binary (§10). Level 3 (OS crash, power loss) is best effort: §6.7
  states the sync order cpf follows; no power-loss outcome is promised or
  tested.
- **Roots outside the guarantee.** Network filesystems and cloud-synced
  folders, as archive roots or workspaces. cpf warns when the root's
  filesystem type is a network type (§4.1); it cannot detect cloud-synced
  folders, and the user documentation says so.
- **What is protected.** After `cpf archive --apply`, every file that was
  under the archived problem directory is, at every crash state of §5–§6,
  in exactly one or more of: its original place (not yet retired), the
  retired tree `.cpforge/archive/<txn>/`, the committed snapshot (bytes
  equal, verified by hash), the quarantine, or the OS trash. Duplicates are
  preferred over loss. The exceptions are exactly:
  1. under `cleanup = "delete"`, bytes written through a file handle opened
     before retirement, after cpf's last check of that file (§6.3); the
     plan states this before `--apply`;
  2. a trash backend that copies instead of renaming, or destroys instead
     of recycling, despite the preconditions of §6.5: writes made through
     a pre-retirement handle during the backend call can be lost, and
     discarded items (not in the snapshot) can be lost (§6.5 fallback rule);
  3. power loss (level 3);
  4. a concurrent rename or link swap of a structural directory (S §1).
- **Not a reversal.** `cpf restore` restores the retained content of a
  snapshot; content a keep profile discarded is not restored, and tool
  state that was removed (facts, job directories) is not recreated.

## 2. Terms and identifiers

- **Archive root**: a directory holding the marker `.cpforge-archive/`
  (§4.1). Snapshots live below it at their layout path (§3.6).
- **Snapshot**: an immutable directory `<root>/<layout path>/<sid>/`
  (§4.3). A **problem snapshot** holds one problem; a **contest snapshot**
  holds a contest directory and references member problem snapshots (§5.6).
- **Snapshot id** `<sid>`: `p-` (problem) or `c-` (contest), then the UTC
  time `yyyymmddTHHMMSSmmmZ` (19 characters, the system clock at the start
  of the snapshot), `-`, 16 lowercase hex digits (64 random bits): 38
  ASCII characters, a portable component (S §2.1). Unique because the
  snapshot directory is published no-clobber (an existing name: new random
  bits). Ordering by id is informative only (clock changes are harmless).
- **Archive transaction** `<txn>`: the job-id grammar (S §6.2), created
  with `create_dir` of `.cpforge/archive/<txn>/` (an existing name: new
  random part). One transaction retires one problem or one contest
  directory, or restores one problem or contest directory.
- **Retired tree**: `.cpforge/archive/<txn>/retired/`, the problem (or
  contest) directory after its retirement rename; the side trees
  `<txn>/fetch/`, `<txn>/submit/`, `<txn>/facts/` (§5.3).
- **Object identity**: Unix device + inode, Windows volume serial + file
  index, read through a no-follow handle (as S §4.4 step 1), always paired
  with the problem identity (or contest directory name) it was recorded
  for. Recovery acts on a path only when the object there has the recorded
  identity.
- **Quarantine entry**: `.cpforge/quarantine/<txn>/` (§6.6).
- **Classes**: §3.3. **Plan**: §3.7. **Shared limits**: §3.5.

## 3. Inventory, classes and the plan

### 3.1 Configuration (`[archive]`, C §1.1 and §7.6)

```toml
[archive]
roots = ["~/cp-archive"]            # every root cpf searches (≤ 16)
default_root = "~/cp-archive"       # absent: the first of roots
layout = "{site}/{problem_id}"      # problem snapshots; {snapshot_id} is the leaf
contest_layout = "{site}/contests/{contest_id}"
keep = []                           # classes; only needed for "submit"
discard = ["tests:sample"]          # explicit; everything unlisted is kept
cleanup = "trash"                   # trash | quarantine | delete | keep
on_existing = "error"               # error | reuse-identical | new
[archive.profiles.minimal]
discard = ["tests:sample", "tests:custom", "stress"]
cleanup = "quarantine"
[archive.quarantine]
retention_days = 30
```

- Layers 1 and 2 (user and workspace files), never the problem layer.
  Arrays are replace-as-unit; a profile's `keep`, `discard` and `cleanup`,
  when present, replace the base values for that run. At most one profile
  per run (`--profile <name>`); an unknown name → 7.
- Paths in `roots`, `default_root` and `--to`/`--root` are absolute or
  start with `~/` (the user's home directory); anything else → 7.
- `keep` and `discard` hold class names of §3.3. A class in both lists of
  one table, `metadata` or `fetch` in `discard`, or an unknown name → 7.
- `cleanup = "delete"` is accepted in configuration and on the command line
  (`--cleanup delete`); it is never a default, and the plan prints the
  warning of §3.7 whenever it applies.
- Defaults: `roots = []`, `layout` and `contest_layout` as above, `keep =
  []`, `discard = []`, `cleanup = "trash"`, `on_existing = "error"`,
  `retention_days = 30` (an integer 0–36 500). No root from any source
  (`--to`, `default_root`, `roots`) → 7 "no archive root".

### 3.2 Sources and walk

The inventory of a problem `problems/<p>/` covers three trees, each walked
lock-free with the S §2 no-follow walker:

| Tree | Path | Notes |
|---|---|---|
| `problem` | `problems/<p>/` | the whole directory |
| `fetch` | `.cpforge/fetch/<p>/` | history and status records (S §5.8.10); absent for local problems |
| `submit` | `.cpforge/submit/<p>/` | submission versions (submission contract) when present |

Facts (`.cpforge/facts/<p>/`) and job directories are not inventoried: facts
are removed at retirement (§5.3), job directories are left to pruning
(S §6.3), and download areas (`.cpforge/downloads/`, keyed by fetch
transaction, not by problem) are never touched: a fetch in its network
phase finds the problem gone at its revalidation (S §5.8.4 step 2). A contest's inventory is `contests/<c>/` alone (tree `contest`).

Entry rules, checked during the walk; any violation refuses the plan
(exit 8) with every offending path listed, before anything is written:

- Entries are regular files and directories. A link (symlink, junction,
  other reparse point) → `link_in_payload`; a FIFO, socket or device →
  `special_file`; an entry on another device or listed as a mount point
  (S §2) → `mount_in_payload`. A regular file with several hard links is
  copied by content; nothing records the other names.
- Every component is a portable component (S §2.1), valid UTF-8, and
  differs from every sibling by collision key; else `unportable_name{path,
  rule}` or `name_collision`. The user renames such files; cpf never does.
- Paths (tree-relative, `/`-separated) are at most 1024 bytes and 32
  components deep; else `path_too_long`.
- The walk stops after 20 000 entries in total → `inventory_too_large`.

### 3.3 Classes

Each file of the `problem` tree gets exactly one class, the first row that
matches; directories have no class (an empty directory is kept as an
entry):

| # | Class | Files |
|---|---|---|
| 1 | `metadata` | `problem.toml`, `tests.toml` (always kept) |
| 2 | `notes` | `notes.md` at the top of the problem directory |
| 3 | `tests:sample` | the input and answer of a registered catalog entry with a `download` table (S §8.1) whose current hashes both equal the recorded download hashes; and any other file under `tests/` whose provenance is `downloaded{role, txn}` (S §5.8.10, decided by the fetch history and an open journal) with role `superseded` or `unreferenced` |
| 4 | `tests:stress` | files of catalog entries the stress contract marks as promoted from a saved failure |
| 5 | `tests:custom` | every other file named by a catalog entry or under `tests/`: tests added by hand, unregistered files, **edited downloads** (a download entry with either hash different or a file missing), and **revision pairs** (provenance role `unregistered-upstream`) |
| 6 | `stress` | everything under `stress/` (saved failures and their binaries, user content once published) |
| 7 | `sources` | the effective `sources`, `support_files` and `solution_files` of the solution target (C §6.2), and every other file whose extension (after the last `.`, ASCII case-insensitive) is in the `extensions` of a configured language |
| 8 | `other` | everything else: untracked, adopted and created files (workspace design §2.2) |

Every file of the `fetch` tree is class `fetch` (always kept); every file
of the `submit` tree is class `submit`. Every file of a contest tree is
`metadata` (`contest.toml`) or `other`.

Rows 3 and 5 make the fetch history, not file names, decide tool
provenance: a file that only looks like a sample is user content unless a
record names it with its current hash. Edited downloads, revision pairs and
saved stress failures are user content and are discarded only when
`tests:custom` or `stress` is discarded explicitly.

### 3.4 Keep and discard

With the effective `keep` and `discard` lists (base, or the profile's
replacement):

- `metadata` and `fetch`: always kept.
- `submit`: kept only when `keep` names it; otherwise **omitted** as tool
  output (it is regenerable from the sources), recorded in `omissions`.
- Any other class: discarded iff `discard` names it; otherwise kept. A file
  of class `other` is always kept: no rule names it, and the plan lists
  each such file by path.
- A discarded file is not in the snapshot. Cleanup sends it to the cleanup
  mode, except that under `delete` it goes to quarantine (§6.2):
  permanent selective deletion without quarantine is not offered.

### 3.5 Shared limits

One constant set in one module is used by the writer (plan, §3.7) and the
reader (restore, §7.2):

| Limit | Value |
|---|---|
| entries in one snapshot (files and directories, all trees) | ≤ 10 000 |
| bytes of one file | ≤ 256 MiB (268 435 456) |
| bytes of all files of one snapshot | ≤ 1 GiB (1 073 741 824) |
| tree-relative path | ≤ 1024 bytes, ≤ 32 components |
| layout | ≤ 8 segments; the expanded layout path ≤ 1024 bytes |
| manifest (§4.4) | ≤ C = 1 048 576 + 10 000 × 1 536 = 16 408 576 bytes |
| summary (§4.5) | ≤ 65 536 bytes |
| `config.json` (§4.3) | ≤ 1 048 576 bytes; larger → omitted with `config_too_large` |

The limits apply to the **written payload**, not to the source: the plan
renders the staged `problem.toml` (the §5.2 step 3 `archived_at` edit,
prepared against the current bytes with a placeholder timestamp of the
fixed 24-character `time` form, so the length is exact) and uses that size
for the per-file and total limits. It also computes the exact encoded
length of `summary.json` and `manifest.json` (every member's value is
known at plan time except `snapshot` and the times, whose lengths are
fixed). It refuses an inventory over any limit
(`snapshot_too_large{limit, value}`, exit 8) before anything is published
or retired, so cpf never creates a snapshot its own restore refuses. At
§5.2 step 3 the staged `problem.toml` must have exactly the planned size,
and at step 5 each document exactly its planned length; a difference
aborts the snapshot (`source_changed`). The writer also refuses to encode
above C (internal error, 70, impossible given the field limits of §4.4).

### 3.6 Layouts

- A layout is a `/`-separated list of 1–8 **segments**; each segment is a
  non-empty mix of literal text and placeholders `{site}`, `{problem_id}`,
  `{contest_id}`, `{label}`, `{contest_date_year}`. Unknown placeholders,
  `{snapshot_id}`, `{tags}`, title slugs, an empty segment, a leading `/`,
  `.` or `..` → 7 at configuration validation.
- **Values**: `{site}` is the identity's site (`local` or the site
  grammar, S §2.1); `{problem_id}` the native part of the identity (for a
  local problem its name); `{contest_id}` the contest's native id (a local
  contest: its directory name) and `{label}` the member's label, both only
  while archiving that contest (§5.6); `{contest_date_year}` a contest's
  start year, which no current adapter records. A value that is unavailable
  is an error (`layout_value_missing{placeholder}`, exit 7), never an empty
  string.
- **Validation**, all before anything is published: each expanded segment
  must be a portable component (S §2.1: separators, `:`, drive and UNC
  forms, control characters, reserved device names, trailing dots and
  spaces, length) and must not start with `.`; values are validated, never
  rewritten. The expanded path is compared with the existing entries of
  each directory along it by collision key; an existing entry with an equal
  key but another spelling → `layout_collision` (exit 8). Names inside the
  payload are validated by §3.2.
- `{snapshot_id}` is always appended as the leaf. The expanded values are
  frozen in the manifest (`layout`), so later tag, title or configuration
  changes never move or rename a snapshot.
- `--to <dir>` replaces the root for one operation. A `--to` root is not
  remembered; the plan warns when it is not in `archive.roots` (later
  listing and search will not find the snapshot).

### 3.7 References, refusals and the plan

The plan of `cpf archive <target>` (target: a problem reference as in
S §5.9.3, or `contest:<name>`) is computed in two parts.

**Under the workspace lock** (S §4.5, with leftover recovery), refused with
nothing written, exit 8 unless stated:

- any job lease in `runs/` is held, or cannot be probed (S §6.2 outcome
  `failed`): `lease_held{jobs}`. This applies to every mode, `keep`
  included. A durable problem binding in `job.toml` is a later design;
  until then a running stress job blocks every archive in the workspace;
- a fetch journal of `<p>` exists (any classification): `fetch_incomplete`;
- an archive transaction naming `<p>` is still `retiring` after recovery
  (§5.3): `archive_incomplete{txn}`;
- `problem.toml` not `valid`, identity not bound to the directory, or
  `tests.toml` neither `missing` nor `valid` (S §9, §2.1);
- configuration errors (7).

The same step records the **references**: whether the saved selection names
the problem; every contest directory whose `contest.toml` lists the
identity (an unreadable, malformed or too-new `contest.toml` counts as
listing it); the object identity of `problems/<p>/` and of each side tree;
the fetch records present (names and hashes).

**Lock-free**: the walk and hashing (§3.2), classes, keep and discard,
limits, the destination (root, §4.1; expanded layout, §3.6; overlap,
§4.2), the existing snapshots at the layout path (§5.7) and the
**retirement decision**: `keep` mode → `export` (snapshot only, nothing
changed); the problem listed by a contest other than the one being archived
→ `snapshot_only{member_of_active_contest, contests}`; otherwise
`retire`.

**Plan binding.** The plan hash is the SHA-256 of the canonical JSON (R
§3.1) of the plan's **binding**, a value that is hashed but not printed
(its member order is fixed in R §6.1): the cpf version, target
identity and directory, the source object identities, every inventory
entry (tree, path, kind, size, SHA-256, executable bit, class, decision),
the effective archive configuration (keep, discard, profile, cleanup,
on_existing, layout), the SHA-256 of the problem's `config.json` (§4.3),
the destination (canonical root as a native value, expanded layout path),
and the references above. Any change of any of these changes the hash.

**Output.** Plan mode prints, and with `--json` writes the `archive_plan`
document (R §6.1): per class the file count, bytes and decision; every
`other` file by path; the destination; the retirement decision and its
reason; the selection and contest references; omissions and externals
(§4.3); the cleanup mode and what it does to matched, discarded and
divergent files; the plan hash. Under `delete` the plan carries the fixed
warning *"delete removes files permanently after their last check; bytes
written after that check through a file handle opened before retirement
are lost."* Plan mode is read-only apart from the leftover recovery of the
lock acquisition (S §4.5, including rule R of §6.7).

`cpf archive <target> --apply` computes a fresh plan and applies it.
`--apply --plan <hash>` refuses (exit 8, `plan_changed`, nothing written,
the fresh plan printed) when the fresh plan's hash differs.

## 4. Archive roots and snapshots

### 4.1 Root guard

- **Resolution.** The root path (§3.1) is expanded, must exist as a
  directory, and is canonicalised once, as the workspace root (S §2).
  Components below the canonical root are checked with `symlink_metadata`
  at use (S §2). cpf never creates the root directory itself.
- **Marker**: `<root>/.cpforge-archive/` containing `root.toml` (`schema =
  1`, tool-only record, S §9 classification) and the lock file `lock`. On
  the first archive to a root, the marker directory is created
  (`create_dir`, an existing directory accepted), then `lock` with
  `create_new` (an existing file accepted), then, under the root lock,
  `root.toml` no-clobber. A root whose `root.toml` is not `valid` → refused
  (8), reported by readers; `too_new` → nothing is written to it.
- **Root lock** (`ArchiveRootGuard`): the S §4.5 lock backend on
  `<root>/.cpforge-archive/lock` (same acquisition bound of 10 s, `Busy`,
  replaced-entry check, Windows byte-range lock). Not reentrant.
- **Root staging**: `<root>/.cpforge-archive/staging/<sid>/`, created only
  under the root lock. **Root recovery** runs right after each root lock
  acquisition, before anything is created: every entry of `staging/` is
  removed with the no-follow walker; a failure is the warning
  `RecoveryIncomplete` and the operation continues.
- **Root publication primitives.** Directory publication is S §4.3
  `publish_dir_noclobber` with its source in the root's staging; file
  publication into an existing snapshot directory (contest markers and
  manifest, §5.6) writes its temporary file in the root's staging and
  renames it no-clobber into place (Linux `renameat2(RENAME_NOREPLACE)`,
  macOS `renamex_np(RENAME_EXCL)`, Windows `MoveFileExW` without replace),
  so a crash never leaves a temporary file outside `staging/`. The whole
  layout path must be on the root's filesystem: `CrossFilesystem` (a mount
  point inside the root) refuses the archive (8), nothing published. These
  primitives take `&ArchiveRootGuard` and never a workspace guard.
- **Network warning**: the plan warns `network_root` when the root's
  filesystem is of a network type (Linux `statfs` magic NFS, SMB/CIFS,
  9P, FUSE; macOS `f_fstypename` `nfs`, `smbfs`, `afpfs`, `webdav`;
  Windows `GetDriveTypeW` = `DRIVE_REMOTE`). The warning never blocks.

### 4.2 Overlap and links

- The canonical root is not the workspace root, not inside it, and not an
  ancestor of it (`root_overlap`, exit 8). Reason: a snapshot inside the
  workspace would be inventoried by later operations, and a root containing
  the workspace would make root traversal (§8) walk the workspace.
- Every directory from the root down to the snapshot's parent is a real
  directory (no link, junction or reparse point); missing ones are created
  one at a time with `create_dir` under the root lock, each checked after
  creation. A link or a non-directory → `link_in_destination` (8).
- Readers and restore open everything below a snapshot directory without
  following links and refuse anything that is not a regular file or
  directory where the manifest expects one (§7.2).

### 4.3 Snapshot directory

```
<root>/<layout path>/<sid>/
    summary.json        bounded summary for listing and search (§4.5)
    config.json         effective configuration as data (optional)
    manifest.json       written last (§4.4)
    problem/...         payload of the problem tree (kept entries)
    fetch/...           fetch history and status records
    submit/...          submission versions, only when kept
```

A contest snapshot replaces `problem/` by `contest/` and adds `intent.json`
and `members/` (§5.6). Nothing in a snapshot is executable by cpf: restore
and search read data only.

- **`config.json`**: the problem's C §10 `config show --json` document,
  stored as data. Larger than 1 MiB → not stored, omission
  `config_too_large`.
- **Externals**: values of the effective configuration that point outside
  the problem directory (template paths, typed paths outside the problem,
  program names of build and run stages), at most 256, as `{kind:
  "program" | "template" | "path", value, key}`; more → `externals_dropped`.
  `key` is the configuration key path (≤ 256 bytes); `value` is the R §3.3
  `nsum(512)` of the native value (≤ 1 173 bytes encoded), so a long or
  non-UTF-8 value is kept as a bounded summary with its hash.
  Restore reports them (§7.3); nothing resolves or runs them.

### 4.4 Manifest codec

The manifest is not a TOML record (the 1 MiB TOML primitives of S §9 do not
apply). It is a JSON document in the R §3.1 canonical writer profile,
`{"schema":1,"document":"archive_manifest",…}`, written and read by one
dedicated module.

| Member | Type | Meaning |
|---|---|---|
| `platform` | token | writer's platform (R §3.3), informative |
| `snapshot` | string | the `<sid>` |
| `kind` | token | `problem` |
| `created_at` | `time` | start of the snapshot; equals the staged `archived_at` |
| `cpf_version` | `ver` | |
| `identity` | string | problem identity (S §2.1; ≤ 256 bytes, longer → the plan refuses `identity_too_long`) |
| `dir` | `ident` | `problems/<dir>` it came from |
| `layout` | `{template: string, values: [{name: token, value: string}; ≤5], path: string}` | the frozen expansion; `path` is root-relative with `/` |
| `plan` | `hex64` | plan hash (§3.7) |
| `profile` | `ident?` | |
| `cleanup` | token | the mode the archive was applied with |
| `contests` | `[{dir: ident, identity: string?, label: string?}; 64]` + `contests_dropped` | contests listing the problem at archive time, in directory order; `identity` ≤ 256 bytes (else `null`), `label` by the S §5.2 grammar (≤ 16) |
| `source_object` | string | object identity of `problems/<p>/` (`unix:<dev>:<ino>` or `windows:<serial>:<index>`) |
| `entries` | `[Entry; ≤10000]` | in tree order (`problem`, `fetch`, `submit`), each tree in path byte order, a directory before its contents |
| `omissions` | `[{what: token, count: u32, bytes: u64}; ≤16]` | discarded classes, `submit` when omitted, `facts`, `runs`, `config_too_large` |
| `externals` | `[External; ≤256]` + `externals_dropped` | §4.3 |
| `summary_sha256`, `config_sha256` | `hex64`, `hex64?` | of `summary.json` and `config.json` |

`Entry`: `{tree: token, path: string, kind: "file" | "dir", class: token?,
size: u64, sha256: hex64?, source_sha256: hex64?, exec: bool}`. For a file,
`sha256` covers the payload bytes and `size` their length; `source_sha256`
is present only for `problem/problem.toml` and holds the hash of the
source bytes (before `archived_at` was set, §5.2 step 3). `exec` is the
Unix owner-execute bit (always `false` when written on Windows). For a
directory `class`, `sha256`, `source_sha256` are `null` and `size` is
`"0"`. Paths contain only portable components, so they need no escaping.

**Field limits** (each enforced by the plan, so every value has a stated
maximum): strings that are identities ≤ 256 bytes; layout `template` ≤
1 024, each layout value ≤ 255, `path` ≤ 1 024; entry paths ≤ 1 024
(portable components need no escaping); tokens ≤ 32; `profile` an
Identifier; externals as §4.3.

**Bound.** An entry serializes to at most 1 293 bytes (path 1 026, the
other members 267 with keys, separators and a comma); it is budgeted at
1 536. Every other member together is at most 1 MiB: 256 externals of at
most 1 536 (key 258 + value 1 173 + kind and keys), 64 contests of at most
400, layout at most 3 500, the rest under 4 KiB. Hence C = 1 048 576 +
10 000 × 1 536 (§3.5); the budget exceeds the true worst case, which
§10.3 serializes. The reader reads at most C + 1 bytes and refuses a
longer file (`manifest_too_large`) before parsing.

**Reader acceptance**: R §3.5 (duplicate members, types, canonical
integers) and §7.2. Unknown members are ignored. A newer `schema` →
`manifest_too_new`: listed, never restored.

### 4.5 Summary

`summary.json`, `{"schema":1,"document":"archive_summary",…}`, at most
64 KiB, written before the manifest: `snapshot`, `kind`, `identity`, `dir`,
`title` (`string?`, ≤ 1024 bytes), `url` (`string?`), `tags` (the valid
`tags` field, else `[]`), `status` (`string?`, the stored status when
valid), `created_at`, `updated_at`, `solved_at`, `archived_at` (`time?`,
S §5.9.2), `contests` (as the manifest's, ≤ 64), `layout_path`, and for a
contest snapshot `members` (count) and `complete` (always `false` in the
summary; completeness is the manifest's presence, §5.6). Field limits:
`title` ≤ 1 024 bytes and `url` ≤ 2 048 (fetch's limits, S §5.8.1), tags
as S §5.9.2 (≤ 64 × 32), contests as the manifest's, identities ≤ 256;
a longer stored `title` or `url` (hand-edited) is cut at a character
boundary with `title_truncated` / `url_truncated` set. The worst case is
below 48 KiB, so the 64 KiB bound cannot be reached. Listing and search
read only summaries (§8). A summary is informative: restore never uses it.

### 4.6 Lock order

The fixed order is **workspace lock, then root lock**: a process holding a
root lock never acquires the workspace lock. In this contract no operation
holds both: the snapshot phase (§5.2) holds the root lock alone, after
releasing the workspace lock; retirement, cleanup finishing, quarantine and
restore hold the workspace lock alone. The bulk copy therefore never blocks
other `cpf` commands of the workspace, and consistency of the copy comes
from hashes (§5.2), not from the lock.

## 5. `cpf archive --apply`

### 5.1 Overview

```
cpf archive <target> [--to <dir>] [--profile <name>] [--cleanup <mode>]
                     [--on-existing <mode>] [--apply [--plan <hash>]] [--json]
cpf archive --resume [--apply] [--json]
cpf archive --close <txn> [--apply]
```

A problem archive runs: the plan (§3.7) → **snapshot** (§5.2, root lock)
→ **retirement** (§5.3, workspace lock) → **cleanup** (§6, under the
transaction's lease). `keep` and `snapshot_only` stop after the snapshot.
Before its own work, every `cpf archive --apply` and `--resume --apply`
runs cleanup recovery for the pending transactions whose lease is free
(§6.7).

### 5.2 Snapshot phase

| Step | Action | Crash point after it |
|---|---|---|
| 1 | Plan under the workspace lock, then lock-free (§3.7); release the workspace lock | — |
| 2 | Root lock, root recovery (§4.1); marker created if absent; existing snapshots at the layout path checked (§5.7); create `staging/<sid>/` | `archive.root_staged` |
| 3 | Copy each kept entry from its source (opened no-follow, must be a regular file of the planned size) into staging, hashing the bytes read; each file `sync_all` (S §4.2 step 1 rules). The hash must equal the plan's, else abort `source_changed{path}`. `problem.toml` is staged with `[problem] archived_at` set to `created_at` (the S §4.4 operation semantics applied to the bytes read, after their hash matched; a result above 1 MiB → abort `record_too_large`). Executable bit applied on Unix | `archive.copied#i` |
| 4 | Re-enumerate the three source trees: names, kinds and sizes must equal the plan inventory (kept and discarded); else abort `source_changed` | — |
| 5 | Write `summary.json`, `config.json`, then `manifest.json`, each synced; sync the staged directories bottom-up | `archive.manifest_written` |
| 6 | Create the missing layout directories (§4.2), then `publish_dir_noclobber` (S §4.3) of `staging/<sid>` to `<root>/<layout path>/<sid>`. **Commit point A** | `archive.published` |
| 7 | Release the root lock | — |

- Every abort before step 6 removes the staging directory (a failure to
  remove it is left for root recovery) and publishes nothing. Disk full,
  permission and I/O errors are reported by class (`disk_full`,
  `permission`, `io`) with the path, exit 8.
- A committed publication whose `Durability` is `Uncertain` (S §4.1) is a
  committed snapshot, but **retirement does not start**: outcome
  `snapshot_only{snapshot_durability_uncertain}` (exit 9). Retirement relies
  on a snapshot that a power loss cannot take back, as far as cpf can make
  it so.
- Edits made by editors after step 3 read a file are not in the snapshot;
  cleanup finds them by hash and quarantines them (§6.2).

| Killed | State | Next run |
|---|---|---|
| before or during step 2–5 | workspace unchanged; root staging may remain | root recovery removes the staging at the next root lock; nothing else to do |
| after step 6 | workspace unchanged; complete snapshot published | the snapshot stays (an orphan, listed like any other); a rerun makes a second snapshot, or reuses it (`reuse-identical`, §5.7) |

### 5.3 Retirement

Runs only for the decision `retire`, after commit point A, under the
workspace lock (with leftover recovery) held from step 1 to step 7:

| Step | Action | Crash point after it |
|---|---|---|
| 1 | Recheck: no job lease held or unprobeable; no fetch journal of `<p>`; `problems/<p>/` has the planned object identity and identity; no contest other than the one being archived lists it now. A failed recheck → no retirement, outcome `snapshot_only{reason}` (exit 9) | — |
| 2a | Create `.cpforge/archive/<txn>/`, create and lock its `lease` (`create_new`, S §6.2 primitive, as a download area, S §5.8.3) | `archive.txn_created` |
| 2b | Publish `plan.json` no-clobber, synced (the plan's inventory: every entry's tree, path, kind, size, SHA-256, executable bit, object identity, class and decision; `archive_plan_inventory`, the §4.4 codec rules, at most 20 000 entries of at most 1 600 bytes, bound 1 MiB + 20 000 × 1 600) | `archive.plan_published` |
| 2c | Publish `journal.toml` no-clobber, synced, with `phase = "retiring"` | `archive.journal_published` |
| 3 | Rename `problems/<p>` → `.cpforge/archive/<txn>/retired` (same filesystem; Windows retries a sharing violation with the S §4.2 step 3 backoff); verify the object identity of `retired`; Unix: sync `problems/` and `<txn>/`. **Commit point B** | `archive.retired` |
| 4 | For each present side tree in the order `fetch`, `submit`, `facts`: rename `.cpforge/<tree>/<p>` → `<txn>/<tree>` and verify its identity | `archive.side_retired#k` |
| 5 | If `state.toml` is `valid` and `current_problem` is the identity: atomic replace with `schema = 1` only (S §5.7); reported `selection_cleared`. A `state.toml` that is not `valid` is left alone and reported `selection_unknown` | `archive.selection_cleared` |
| 6 | Delete `<txn>/facts` with the no-follow walker (facts are derived records, S §6.5; a failure is retried by cleanup) | `archive.facts_removed` |
| 7 | Replace the journal (S §4.2) with `phase = "retired"`; release the workspace lock; cleanup follows (§6) under the lease | `archive.phase_retired` |

- **Journal** (`journal.toml`, schema 1, tool-only, unknown keys
  `malformed`): `kind` (`problem` | `contest` | `restore`), `txn`, `phase`,
  `identity`, `dir`, `snapshot` (id), `snapshot_path` (canonical root as a
  string plus the root-relative path; non-UTF-8 roots are written in the R
  §3.3 `ns` form), `manifest_sha256`, `cleanup` (mode), `source_object`,
  `[[side]]` (`tree`, `object`), `clear_selection` (bool, the step-1
  observation). It is far below the 1 MiB cap.
- A failure of step 3 (any error, including `CrossFilesystem` when
  `problems/` and `.cpforge/` are on different filesystems, and the Windows
  "directory in use" error when any process keeps a file or current
  directory open inside the problem) removes the journal and the
  transaction directory: outcome `snapshot_only{retire_failed}` (exit 9),
  problem unchanged and still selected.
- A failure of steps 4–7 (not a crash) leaves the transaction `retiring`;
  the next locked operation rolls it forward (§6.7 rule R). The command
  reports `retirement_incomplete` (exit 9).
- Contest directories (§5.6) use the same steps with `kind = "contest"`,
  the tree `contests/<c>`, no side trees and no selection.
- Nothing edits any `contest.toml`: membership changes only through a
  contest archive, which retires the whole contest directory.

| Killed after | State | Recovery (next locked operation, §6.7) |
|---|---|---|
| step 2a or 2b | transaction directory without `journal.toml` | **initialization leftover**: the directory is removed with the no-follow walker (no user file can be in it: the problem moves only at step 3, after the journal exists) |
| step 2c, before step 3 | journal `retiring`, problem in place | **roll back**: `problems/<p>` has the recorded object identity and `retired` is absent → journal and transaction directory removed; outcome left: snapshot only |
| step 3 | problem in `<txn>/retired`, side trees in place, selection unchanged | **roll forward**: steps 4–7 |
| each rename of step 4 | some side trees moved | roll forward the rest: a source path with the recorded identity is moved; an absent source with the target present is done; a source holding another object is left alone (reported) |
| step 5 or 6 | selection cleared, facts present or deleted | roll forward |
| step 7 | `phase = "retired"` | cleanup recovery (§6.7) |

### 5.4 Outcomes

Per problem: `retired` (cleanup `complete` → 0, `incomplete` → 9),
`exported` (`keep`, 0), `reused` (no new snapshot, §5.7, then as
`retired`), `snapshot_only{reason}` (9), refused before the snapshot (7 or
8). `--resume` and `--close`: 0 when every transaction they touched ended
`complete`, 9 otherwise, 8 when nothing could be locked or read. Snapshot
success is always reported separately from retirement and cleanup failures
(disk full, permission, sharing, trash unavailable).

### 5.5 `--close <txn>`

For a transaction whose cleanup cannot finish (unsettled items, §6.3,
§6.5; a source missing in `retiring`; a malformed journal; entries left
after the last sweep): plan by default, `--apply` acts. Under the
workspace lock, with the lease free (a held lease → refused, 8):

1. publish `<txn>/closing` no-clobber and synced (crash point
   `archive.closing`): the **closing intent**, a tool-only TOML record
   (schema 1) listing each remaining top-level entry of the transaction
   directory other than cpf's records (`lease`, `journal.toml`,
   `plan.json`, `cleanup.json`, `sweep-*`, `trash-*`, `redirect/`,
   `closing`, `finishing`), i.e. among `retired`, `fetch`, `submit`, `facts`,
   `trash`, `delete`, each with its kind and object identity, and the
   identity and snapshot copied from the journal when it is `valid`
   (otherwise absent). It does not depend on the journal;
2. for each listed entry: if `.cpforge/quarantine/<txn>/files/close/<name>`
   holds its object → done; if `<txn>/<name>` holds it → rename it
   no-clobber there (one subtree item each, at most 6; crash point
   `archive.close_moved#k`); neither → reported `vanished`;
3. finish as §6.4 steps 2–3: the record lists the subtree items with their
   counts and every earlier unsettled item, and teardown removes `closing`.

A transaction holding `closing` is continued from step 2 by
`--close --apply` or by cleanup recovery (§6.7) **before** its journal or
phase is looked at, so a malformed journal or a `retiring` phase never
stops it; rule R neither removes it as a leftover nor rolls it forward.
Nothing is deleted.

### 5.6 Contests

`cpf archive contest:<c>` archives the contest directory and its members
(the problems its `contest.toml` lists, in record order, at most 256; an
unresolvable member → refused, 8, listing it, unless a contest run of that
contest is open, below).

1. **Plans**: one per member (with `{contest_id}` and `{label}` available)
   and one for the contest tree; every refusal of any plan refuses the whole
   archive before anything is written. The lease rule applies once.
2. **Contest snapshot, incomplete**: root lock; stage `c-…/` with
   `intent.json` (`archive_contest_intent`: contest identity and directory,
   the planned members `{label, identity, dir, plan}` in order, the contest
   plan hash) and `contest/` (the contest tree), synced; publish it
   no-clobber at the contest layout path (crash point
   `archive.contest_published`). It has no manifest yet: listing shows it
   `incomplete`.
3. **Members**, in order: the snapshot phase of §5.2 (each its own root
   lock), then, under the root lock, a **member marker**
   `members/<dir>.json` (`archive_member`: identity, member `<sid>`, its
   layout path, its manifest hash, its `source_object`) published
   no-clobber into the contest snapshot (crash point
   `archive.member_marked#m`).
4. **Contest manifest** (`archive_contest_manifest`): the contest tree's
   entries (§4.4 `Entry`, tree `contest`), and the members `{label,
   identity, dir, snapshot, path, manifest_sha256}` in order; published
   no-clobber as `manifest.json` into the contest snapshot after every
   member marker exists (crash point `archive.contest_manifest`). Its
   presence is the **completion marker**; from then on the contest
   snapshot is immutable.
5. **Contest run**, only after step 4, under the workspace lock: create
   `.cpforge/archive/<ctxn>/` with a `journal.toml` of `kind =
   "contest_run"` (published no-clobber, synced; crash point
   `archive.contest_run`): the contest directory and its object identity,
   the contest snapshot (id, path, manifest hash), and per member `{dir,
   identity, object, snapshot, manifest_sha256}` from the markers. It binds
   the rest of the archive to these objects and snapshots; it holds no user
   file.
6. **Retirement**: each member by §5.3 with its own transaction, in order
   (crash point `archive.member_retired#m` after each member's commit point
   B); a member listed by another contest is
   `snapshot_only{member_of_active_contest}` and stays active. Then the
   contest directory (`kind = "contest"`, crash point
   `archive.contest_retired`) when every member was retired or stays active
   only because another contest lists it; any other member outcome keeps
   the contest directory (exit 9). The contest-run directory is removed
   (journal last) only when the contest directory has been retired, or by
   `cpf archive --close <ctxn> --apply` (it holds only cpf records); after
   any other outcome, including an ordinary non-crash failure of one
   member's retirement, it stays open and the next `cpf archive
   contest:<c> --apply` or `--resume --apply` resumes it.

**Resuming a contest run.** `cpf archive contest:<c> --apply` and
`cpf archive --resume --apply` resume an open contest run of that contest
(a `contest_run` journal naming it) instead of planning anew, without
applying `on_existing` again and without requiring the members to resolve.
Per member, by identity: `problems/<dir>` holds the recorded `object` →
retired now by §5.3 against the recorded snapshot (cleanup revalidates
against it); `problems/<dir>` absent → already retired (its own transaction,
if still open, is handled by §6.7) or removed by hand: nothing to do,
reported `member_absent`; `problems/<dir>` holds another object → left
alone, reported `member_replaced`, and the contest directory is kept. The
contest directory is retired only if it still holds its recorded object.
Each resumed step is idempotent, so a kill at any of the crash points
above followed by a resume ends in the same state as an uninterrupted run.

**Rerun before the manifest.** Without an open contest run, a rerun of
`cpf archive contest:<c> --apply` looks for an
incomplete contest snapshot at the contest layout path whose
`intent.json` names the same contest identity and the same member list
(labels, identities, directories). If one exists it is **resumed**:
members with a marker whose snapshot manifest is present and valid are not
snapshotted again (the plan lists them with their age; their retirement is
revalidated against that snapshot by cleanup); the other members run step
3; then steps 4–6. Otherwise a new contest snapshot is made and the old one
stays `incomplete`. A member snapshot published without its marker (killed
between them) is an orphan problem snapshot; the rerun makes another.

| Killed | State | Rerun / restore |
|---|---|---|
| before step 2 commits | nothing in the root (staging recovered) | starts afresh |
| after step 2, during step 3 | incomplete contest snapshot, some member snapshots and markers; workspace unchanged | resumes; restore without `--partial` refused |
| after step 4, before step 5 | complete contest snapshot; no member retired; no contest run | a rerun is a new archive: it reuses the complete snapshot when `on_existing = "reuse-identical"` and every member is equivalent (§5.7), else follows `on_existing` |
| after step 5, during or after step 6 | contest run open; some members retired | each member transaction by §6.7; the contest run is resumed as above |

### 5.7 Existing snapshots (`on_existing`)

An **existing snapshot** is a complete snapshot (manifest present and
`valid`) of the same identity directly under the expanded layout path.
Checked under the root lock at §5.2 step 2:

- `error` (default): refused, exit 8, `snapshot_exists{snapshots}`; nothing
  written. `--on-existing new` overrides it for one run.
- `new`: a new snapshot is made (ids are unique; nothing is merged).
- `reuse-identical`: if the newest existing snapshot (by `created_at`, then
  id) is **equivalent**, no new snapshot is written and that snapshot is the
  one retirement and cleanup use (outcome `reused`); otherwise as `error`.
  Equivalent means, after reading and validating its manifest (§7.2): the
  same identity, directory and kind; the same kept entries (tree, path,
  kind, size, executable bit), each file's `sha256` equal to the planned
  hash (`source_sha256` for `problem.toml`), and every file's payload bytes
  rehashed equal to its `sha256`; the same omissions by class; the same
  `config_sha256`. `archived_at`, `created_at`, layout values, plan hash,
  profile and cleanup mode are not compared.

## 6. Cleanup

### 6.1 Principles

Cleanup works only inside `.cpforge/archive/<txn>/`; it never looks at
`problems/<p>` again. A path there that an editor creates after retirement
(an editor saving to the old path recreates `problems/<p>/` without
`problem.toml`) is reported by `cpf status` and `cpf fsck` and never
removed. Cleanup runs under the transaction's lease, without the workspace
lock, except for its last step.

### 6.2 Revalidation and decisions

Leftovers of `<txn>/facts` (§5.3 step 6) are deleted first. Revalidation reads
the snapshot manifest at `snapshot_path` (§7.2 validation;
its SHA-256 must equal `manifest_sha256`). An unavailable or changed
manifest (root unmounted, unreadable) stops cleanup with nothing moved:
`incomplete{snapshot_unavailable}`, retried by the next cleanup recovery.
Then every entry of the retired trees is walked (no-follow) and decided:

| Entry found | Condition | `trash` | `quarantine` | `delete` |
|---|---|---|---|---|
| regular file, tree `problem` | in the manifest; size, executable bit and SHA-256 (`source_sha256` for `problem.toml`) equal | trash | quarantine `cleanup_mode` | delete |
| regular file, tree `problem` | discarded by the plan (`<txn>/plan.json`); size, bit and hash equal the plan's | trash | quarantine `cleanup_mode` | quarantine `delete_discarded` |
| regular file, tree `fetch` or `submit` | in the manifest and equal, or (`submit` omitted) equal to the plan | delete | delete | delete |
| regular file | any other content or mode | quarantine `divergent` | quarantine `divergent` | quarantine `divergent` |
| any entry | not in `<txn>/plan.json` | quarantine `new` | quarantine `new` | quarantine `new` |
| link, special file, or a kind other than planned | | quarantine `type_changed` | same | same |
| regular file that cannot be opened or read | | quarantine `unreadable` | same | same |
| directory | | removed when empty after its contents (§6.4) | same | same |

Fetch and submit records are cpf's own records whose bytes are in the
snapshot (or, for omitted submit versions, regenerable output), so they are
deleted rather than trashed. Everything not matched goes to quarantine in
every mode.

**Records and the lock.** Every publication or removal of a record inside
`.cpforge/archive/<txn>/` or `.cpforge/quarantine/<txn>/` (`cleanup.json`,
`sweep-<n>.json`, `trash-started`, `trash-result.json`, `redirect/*.toml`,
`record.json`, `finishing`, `closing`) happens under a short workspace-lock acquisition,
so the S §4.5 temporary-file recovery, which runs under that lock, never
meets a live temporary file. Moving items needs no lock.

**Items.** An item is `{pass, tree, path, kind, object, size, sha256?,
decision, reason}`: `object` is the entry's object identity read when it
was decided. Only **recordable** entries become single items: a path of
portable components (S §2.1), valid UTF-8, ≤ 1 024 bytes and ≤ 32
components. An entry that is not recordable (a name created after
retirement) is covered by its nearest recordable ancestor directory, which
becomes one **subtree item** (decision quarantine, reason
`unrecordable_name`) and is not walked further. When a walk would produce
more than its item bound, each tree root (`retired`, `fetch`, `submit`)
instead becomes one subtree item (reason `too_many_entries`). An item
encodes to at most 1 600 bytes.

The first pass's decisions are written as `<txn>/cleanup.json`
(`archive_cleanup`, the §4.4 codec rules, items in walk order, at most
20 000, bound 1 MiB + 20 000 × 1 600), no-clobber and synced, before the
first move (crash point `archive.cleanup_planned`). It is the item
journal: every item is recorded, with its object identity, before it
moves.

### 6.3 Moves

In record order, each item at most once. Every move is **no-clobber**
(S §4.3: Linux `renameat2(RENAME_NOREPLACE)`, macOS
`renamex_np(RENAME_EXCL)`, Windows `MoveFileExW` without replace; never the
link-and-unlink fallback) and is made only after the source path is checked
to hold the item's recorded `object`:

- **quarantine**: to `.cpforge/quarantine/<txn>/files/<pass>/<tree>/<path>`
  (parents created as needed). Each pass has its own directory, so a file
  recreated under a name already moved goes to a later pass's path and can
  never replace an earlier generation.
- **trash** (pass 1 only): to `<txn>/trash/<bundle>/<tree>/<path>`, where
  `<bundle>` is `<dir>.<sid>` (a portable component); the bundle goes to the
  OS trash as one item (§6.5).
- **delete**: rename the file no-clobber to `<txn>/delete/<pass>/<tree>/<path>`
  (a directory no process outside cpf has a reason to use), then, on the
  moved file: open it no-follow, check its identity against `object` and
  rehash it (barrier `archive.delete_moved#i` sits between the rename and
  this check; crash point and barrier `archive.before_unlink#i` sit between
  the hash and the unlink); both equal → unlink it there. Otherwise it is
  **redirected**: another object (a replacement created at the old name
  before the rename, reason `new`) or the same object with other content
  (reason `divergent`). A file created at the old name after the rename is
  a new entry for the next sweep, so no replacement generation is ever
  unlinked.
- **Redirect record.** Before a redirected file leaves `delete/`, cpf
  publishes, under the workspace lock, `<txn>/redirect/<i>.toml` (tool-only,
  schema 1, no-clobber, synced; `i` the delete item's index; crash point
  `archive.redirect_recorded#i`) with the item, the file's **actual**
  object identity, size, SHA-256 and reason, and its destination
  `.cpforge/quarantine/<txn>/files/redirect/<i>/<tree>/<path>`; then moves
  it there no-clobber (crash point `archive.redirected#i`). Settlement of a
  redirect record is by identity like any move (destination holds the
  recorded object → quarantined; `delete/` holds it → move it now; neither
  → unsettled `vanished`). A delete item whose path has a redirect record of
  **another** object is never *deleted*: its own object was not seen by
  cpf after the rename, so it is unsettled (`replaced`); with a redirect of
  its own object (`divergent`) it is quarantined. Redirected files are
  quarantine items of pass `redirect` in `record.json` (§6.6), at most one
  per delete item.
- **Recovery** treats every file found under `<txn>/delete/` the same way,
  whether or not the live process got past the rename: first every redirect
  record is replayed; then each remaining file is opened no-follow and
  checked for identity and hash against the item whose path it has; only a
  match is unlinked; a file at a delete item's path with another object or
  other bytes gets that item's redirect record and is redirected; a file at
  a path **no delete item names** (only possible from outside cpf) is left
  in place, unsettled (`unexpected_in_staging`), and keeps the transaction
  `incomplete` until `--close`, which moves `delete/` as one subtree;
  nothing under `delete/` is ever unlinked unchecked.
- **Pruning.** After either removal from `delete/` (unlink or redirect),
  the now empty directories of that file's path under `<txn>/delete/` are
  removed bottom-up (`rmdir`, stopping at the first non-empty one; crash
  point `archive.delete_pruned#i`). Recovery repeats this pruning for every
  empty directory it finds there.

**Settlement by identity.** An item is *moved* when its destination holds
its `object` (for a `delete` item: once it is gone from
`<txn>/delete/…` after being there); *pending* when its source path, or for
a `delete` item `<txn>/delete/…`, holds it; otherwise its object is gone
from both: a `delete` item is then *deleted*, a `trash`
item is settled by `trash-result.json` (§6.5), and a quarantine item is
*unsettled* (`vanished`: something outside cpf removed it). A source path
holding another object is never acted on for that item; the next sweep
decides it. A no-clobber `Exists` at a destination (only possible if
something outside cpf wrote into the quarantine) leaves the item pending
and unsettled (`destination_taken`).

Renames stay inside `.cpforge/` (one filesystem, checked at retirement),
so they move the inode: bytes written later through a handle opened before
retirement stay in the moved file, in trash or quarantine. Under `delete`
such bytes written after the rehash are lost (§1 exception 1). Crash point
`archive.item_moved#i` follows each move. Unix syncs each directory that
lost or gained entries once, after the last move into or out of it in the
pass. After the last move of pass 1 a non-empty bundle is sent to the
trash (§6.5); then §6.4 follows.

### 6.4 Sweep and finish

1. **Sweep passes** 2 and 3: walk the retired trees again; every entry that
   is not a pending item of an earlier pass (created during cleanup, for
   instance by a process whose current directory is inside the retired
   tree, or a new generation of a moved name) becomes an item of this pass
   (quarantine, reason `new`), recorded in `<txn>/sweep-<n>.json` (same
   codec, at most 4 096 items, bound 1 MiB + 4 096 × 1 600) before its
   moves (crash point `archive.sweep_planned#n`). Then empty directories are
   removed bottom-up (`rmdir`). Pass 3 runs only if pass 2 left a
   non-empty directory; anything left after pass 3 makes the transaction
   `incomplete` (crash point `archive.swept`).
2. **Record**, under the workspace lock: when the quarantine entry exists,
   publish `record.json` (§6.6) no-clobber (crash point
   `archive.record_published`). The record is a pure function of the
   transaction's records and the quarantine's contents; if `record.json`
   already exists (a finish interrupted after it), its bytes must equal the
   recomputed record, else the transaction stops `incomplete`
   (`record_mismatch`) and nothing is removed.
   Before the record is computed, `<txn>/delete/` and `<txn>/trash/` are
   pruned bottom-up completely; any file left in either (a pending item)
   keeps the transaction `incomplete`, so neither directory holds anything
   but empty directories when `finishing` is published.
3. **Teardown**, under the same lock acquisition: publish `<txn>/finishing`
   no-clobber (crash point `archive.finishing`); release and close the
   lease; remove the (empty) `delete/` and `trash/` directories; unlink the
   `redirect/*.toml` records and remove `redirect/`; unlink, in
   this order, `lease`, `plan.json`, `cleanup.json`, the `sweep-*`
   records, `trash-started`, `trash-result.json`, `closing`,
   `journal.toml`, and `finishing` last (crash point `archive.teardown#k`
   after each unlink; each absent file is skipped), then `rmdir` the
   transaction directory. Any other entry left in it stops the teardown
   and is reported (it cannot occur: user content has left the directory
   before the record is published). Lease probes run under
   the workspace lock too (§6.7), so no probe holds the lease file open
   while it is removed. A transaction directory holding `finishing` (with
   or without a journal) is torn down by rule R (§6.7); an empty one is
   removed by rule R.

**Terminal states.** `complete`: the transaction directory is gone and
every item of every pass is in exactly one of *trash*, *quarantine*,
*deleted* or *snapshot-only* (gone from the workspace, bytes held by the
snapshot). `incomplete{items}`: the transaction directory remains with its
journal; the list names every item still pending, every `unsettled` item
and the reason for each. A later cleanup recovery advances it or reports
the same list; `--close` ends it.

### 6.5 Trash backend

**Dependency.** `trash = "=5.2.9"` with its default features
(`coinit_apartmentthreaded`, `chrono`); 5.2.9 declares `rust-version =
"1.85.0"`. On macOS cpf sets `DeleteMethod::NsFileManager` through
`TrashContextExtMacos::set_delete_method` on its own `TrashContext`; the
crate's default `Finder` method runs `osascript`, a process, and is never
used. A dependency review (Linux mount query, the macOS `objc2` 0.6 and
`objc2-foundation` 0.3 graph and the Windows `windows` 0.62 crate on Rust
1.85, binary size) precedes its addition.

**Observed backend behaviour of 5.2.9** (from its source):

| OS | Call | Behaviour cpf relies on or guards against |
|---|---|---|
| Linux, BSD | freedesktop | home trash (`$XDG_DATA_HOME/Trash`, default `~/.local/share/Trash`) when the item's mount point equals the home trash's (a rename); else the per-volume `$topdir/.Trash/$uid` or `$topdir/.Trash-$uid` (a rename); when those cannot be created, the home trash by **copy and remove** across filesystems. Mount points are read with non-thread-safe libc calls behind a crate mutex |
| macOS | `NSFileManager trashItemAtURL` | moves the item to the Trash of its volume; an error leaves it in place; a path that is not UTF-8 is percent-encoded by the crate (a different name) |
| Windows | `IFileOperation` with `FOF_NO_UI \| FOF_ALLOWUNDO \| FOF_WANTNUKEWARNING` | recycles on the item's volume; an aborted operation is a generic error; whether a volume without a usable Recycle Bin silently deletes is not documented |

**Preconditions**, checked **during revalidation, before `cleanup.json`
is published**: if one fails, trash is **unavailable** for the transaction
and the items that would go to the trash are recorded in `cleanup.json`
directly as quarantine items with reason `trash_unavailable{cause}`, so the
redirection is durable before anything moves. They are checked again at
step 1 below; if one fails then (the environment changed meanwhile), the
backend is not called and step 4 publishes a result with every bundle item
`fallback` (`trash_unavailable{cause}`) before any item leaves the
bundle. The preconditions:

- the bundle path is valid UTF-8 (macOS, Windows), and its parent
  directories are real directories (§4.2 rule);
- Linux and BSD: `HOME` or `XDG_DATA_HOME` gives the home trash path `T`,
  resolved as 5.2.9 resolves it; 5.2.9 then takes its home-trash branch
  only when the bundle's mount point equals the mount point of `T`, so that
  equality is required (mount point: the longest mount point of
  `/proc/self/mountinfo`, or `getmntinfo` on BSD, that is a prefix of the
  canonical path). The rename destination is `T/files/`: it and `T/info/`
  must be real directories (created if absent; never links) cpf can write
  (a no-clobber probe file), and **`T/files/` itself** must have the
  bundle's device id and the bundle's mount point (a mount on `T/files/`
  or below `T` fails this). Only then does the crate's rename stay on one
  mount, so it never reaches its copy-and-remove path.
  Per-volume trash directories are never relied on: their failures can
  send 5.2.9 to its cross-filesystem copy-and-remove fallback. Otherwise:
  `cross_filesystem` or `home_trash_unusable`;
- cpf calls the crate from one thread, and no other cpf thread reads mount
  tables through libc while it runs.

**Call and reconciliation.**

1. Under the workspace lock, publish `<txn>/trash-started` no-clobber
   (synced) naming the bundle, its object identity and its item count
   (crash point `archive.trash_marked`).
2. Without the lock: `TrashContext::delete(bundle)` (one item). Crash point
   `archive.trash_called` follows its return.
3. **Reconcile** by looking at the bundle path, whatever the call returned,
   and give every bundle item exactly one outcome:
   - bundle absent → every item `trash` (an error return with the bundle
     gone is reported `trash_error_after_move`);
   - bundle present, its own object, holding every item moved into it → the
     call moved nothing: before any result is published, one retry of
     step 2 when it returned a sharing violation (Windows) within the
     S §4.2 step 3 backoff; otherwise every item `fallback`;
   - bundle present with some items missing → **partial**: items still in
     it `fallback`; a missing item whose bytes are in the snapshot
     `snapshot_only` (`trash_unverified`); a missing discarded item
     `unsettled` (`absent_after_trash`).
4. Under the workspace lock, publish `<txn>/trash-result.json`
   (`archive_trash_result`: the bundle item paths with their outcomes,
   bounded as `cleanup.json`) no-clobber and synced (crash point
   `archive.trash_result`). **This publication settles the trash items**;
   nothing is inferred from absence after it.
5. Move each `fallback` item still in the bundle (by identity) to
   quarantine pass 1 with reason `trash_fallback`; then remove the empty
   bundle directories.

A crash after step 1 without a result is reconciled by the next cleanup
recovery exactly as step 3 (a complete bundle may be trashed again, step 2,
only then), and step 4 is published before anything else proceeds. With a
result present, recovery only performs step 5 and never calls the backend.

**Fallback to quarantine-only.** The trash backend of an OS is replaced by
quarantine (the default `trash` then acts as `quarantine` on that OS, and
the user documentation says so) if the dependency review fails, or if a
test of §10.6 observes on that OS any of: a copy instead of a rename (the
bundle's object identity differs in the trash, or a write through a handle
opened before the call is missing), a permanent deletion reported as
success while the volume's Recycle Bin is enabled and large enough for the
bundle, or a partial move reported as success. A volume whose bin the user
disabled, or one too small for the bundle, may make the shell destroy the
bundle: that is §1 exception 2, stated in the user documentation; cpf
cannot query either setting before the call.

### 6.6 Quarantine

```
.cpforge/quarantine/<txn>/
    files/<pass>/<tree>/<path>   the quarantined entries, as they were;
                                 <pass> is 1, 2, 3 or close
    files/redirect/<i>/<tree>/<path>   files redirected out of delete/ (§6.3)
    record.json                  published last (§6.4 step 2)
.cpforge/quarantine/.purge-<txn>/   being purged
```

- **Record** (`quarantine_record`, §4.4 codec rules): `id` (`<txn>`),
  `created_at` (the earliest `created_at` its inputs record, so a
  recomputation gives the same bytes), `origin` (`{kind: "archive",
  identity, dir, snapshot, snapshot_path}`), `items` (every quarantine item
  of `cleanup.json`, `sweep-*.json`, `trash-result.json` fallbacks,
  redirect records (pass `redirect`) and `--close`, and every unsettled
  item of any decision, **each item once**:
  `{pass, tree, path, kind, size, sha256?, reason, outcome}` with `outcome`
  `quarantined` or `unsettled`, a subtree item with `files: u64, bytes:
  u64` counted at the record's computation). There is no separate
  unsettled list. At most 20 000 + 2 × 4 096 + 6 items of the passes plus
  at most one redirect per delete item (≤ 28 192), each encoding to at most
  1 600 bytes (path 1 026, the other members with keys at most 420): bound
  1 MiB + 56 390 × 1 600. Reasons: `cleanup_mode`, `delete_discarded`, `divergent`, `new`,
  `type_changed`, `unreadable`, `unrecordable_name`, `too_many_entries`,
  `trash_unavailable`, `trash_fallback`, `close`, `vanished`,
  `destination_taken`, `absent_after_trash`, `replaced`,
  `unexpected_in_staging`.
- An entry without `record.json` whose transaction is still open is
  **pending**; one without a record and without a transaction is
  `record_missing`; a record that is not `valid` is reported by its
  classification.
- Quarantined files are user content in a tool tree: cpf never modifies
  them, and removes them only by `cpf quarantine purge --apply` or never.

**`cpf quarantine ls`**: lock-free reader (S §5.9.9 rules): one line per
entry by id: state (`ready`, `pending`, `record_missing`, or the record's
classification), `created_at`, age in whole days, origin identity and
snapshot, item and byte counts, the unsettled count. Exit 0; 8 when
`.cpforge/quarantine/` cannot be enumerated.

**`cpf quarantine purge [<id>…] [--apply]`**: plan by default.
Selection: the named ids; without ids, every `ready` entry whose age is at
least `retention_days`: age = now − `created_at` in milliseconds, selected
iff age ≥ `retention_days` × 86 400 000 (so `retention_days = 0` selects
every ready entry, and a `created_at` in the future is never selected).
A pending entry is never selected, even by id (8 when named). An entry
without a valid record is selected only by id. `retention_days` is applied
only here: nothing purges automatically. With `--apply`, under the workspace
lock (with recovery): rename each selected entry to `.purge-<txn>` (crash
point `purge.renamed`); release the lock; delete each with the no-follow
walker (crash point `purge.deleting#i` after each file). Leftover recovery
(S §4.5) deletes `.purge-*` entries; an entry vanishing meanwhile is
success. Exit 0; 9 when a selected entry could not be renamed or deleted;
8 on lock or enumeration failure.

| Killed | State | Next locked operation |
|---|---|---|
| before the rename | entry intact | — |
| after the rename, during deletion | `.purge-<txn>` partly deleted | recovery deletes the rest |

### 6.7 Recovery

**Rule R (retirement roll-forward and teardown)**, part of every locked
operation's leftover recovery (S §4.5), for every `.cpforge/archive/<txn>/`
in id order:

- holding `finishing` → teardown continues (§6.4 step 3; only cpf records
  remain once `finishing` exists);
- empty, or without `journal.toml` and without `finishing` and `closing`
  → an initialization leftover (§5.3 steps 2a–2b, §7.3 step 3), removed
  with the no-follow walker;
- holding `closing` → never acted on here (cleanup recovery continues it);
- journal `valid` with `phase = "retiring"` → the rows of the §5.3 crash
  table;
- a `restore` journal → §7.3's table;
- a `contest_run` journal (§5.6) → never acted on here.

For `retiring`: It renames only cpf's side trees, replaces `state.toml`
and deletes facts; it never moves a user file (the problem directory either
was moved by the live owner, commit point B, or the transaction is rolled
back). If `retired` is absent and `problems/<p>` is absent or holds another
object, nothing is touched and the transaction is reported
`incomplete{source_missing}` (only `--close` ends it). A `restore`
transaction is decided by §7.3's table under the same rule.

**Cleanup recovery**, run by `cpf archive --apply` (any target) and
`cpf archive --resume --apply`, never by other commands: for every
transaction holding `closing` (whatever its journal) or with `phase =
"retired"`, in id order, probe its lease (S §6.2
outcomes; `held` → skipped, the owner is alive; `failed` → skipped and
reported). The probes run under the workspace lock; a probe that acquires
the lease is **kept as the new owner handle** (unlike job and download
leases, a transaction lease is taken over), so a concurrent recovery sees
it `held`. Then, without the workspace lock:

1. `closing` present → `--close` continues (§5.5);
2. `trash-started` without `trash-result.json` → reconcile and publish the
   result (§6.5 steps 3–4) before anything else; with a result → §6.5
   step 5;
3. `cleanup.json` absent → revalidation (§6.2) from the retired trees as
   they are now;
4. otherwise every item of `cleanup.json` and of each `sweep-<n>.json` is
   settled by identity (§6.3); pending items are moved now (a `delete` item
   is rehashed first); a pass whose record exists is finished before the
   next pass is planned; a trash item is never inferred to be in the trash
   from its absence: only `trash-result.json` says so. Then §6.4 from the
   first step not yet done.

A journal that is `malformed`, `unreadable` or `too_new` is never acted on:
the transaction is reported, and since its directory name cannot be relied
on, it blocks every `cpf archive --apply` and `cpf restore` of the
workspace until `--close` (or a newer cpf for `too_new`). Adding and
fetching are not blocked: a retired tree lives only inside its transaction
directory.

**Order** at a workspace lock acquisition: (1) S §4.5 staging and temporary
files, (2) download-area reclamation (S §5.8.3), (3) `.purge-*` deletion,
(4) rule R; then, only in their commands: (5) cleanup recovery (`cpf
archive`), (6) fetch journal reconciliation (`cpf fetch`, `cpf fsck
--apply`, S §5.8.7). Root recovery (§4.1) runs at each root lock
acquisition. The stores are disjoint: a fetch journal and a `retiring`
transaction of the same directory cannot coexist (§3.7, §5.3), so the order
between (4) and (6) never changes an outcome; staging comes first because
a dead restore's staging is never reused (§7.3).

**Process kill and power loss.** The tables of §5–§7 are the guarantee for
a killed process (level 2). For power loss (level 3) cpf syncs, on Linux
and macOS (macOS also `F_FULLFSYNC`, best effort): each snapshot file
before publication, the staged directories bottom-up, the published
snapshot's parent and new layout directories (S §4.3); the journal before
commit point B, and `problems/` and `<txn>/` after it; `cleanup.json`
before the first move; the directories touched by moves before the sweep;
the quarantine record before the journal is removed. Windows flushes each
written file before its rename and flushes no directory. Retirement never
starts on a snapshot whose publication reported `Durability::Uncertain`
(§5.2). After a power loss, states outside these tables can occur (a
reappeared journal, a rename undone); recovery still decides them by
journal, object identity and presence, but no test covers them and no
outcome is promised.

## 7. Restore

### 7.1 Command

```
cpf restore <snapshot> [--into <dir>] [--partial] [--root <dir>]… [--json]
```

`<snapshot>` is a snapshot id, looked up in every root of `archive.roots`
and `--root` (traversal of §8, its bounds and its containment; the same id
in several roots → refused, 8, listing them, unless `--root` narrows it),
or a path to a snapshot directory (its last component must have the
snapshot id grammar; no root marker is needed). Restore needs no `--apply`:
it never overwrites or removes anything. It runs no hook, does not change
the selection and does not consume the snapshot.

### 7.2 Manifest validation (pure)

The reader of §4.4 plus these checks, each an error naming the entry
(`manifest_invalid{reason}`, 8), before anything is written:

- `schema` supported, `document` and `kind` as expected; `snapshot` equals
  the directory name; `identity` valid (S §2.1) and `dir` the directory its
  identity derives (local: the name; remote: `<site>-<native>`);
- every path relative, `/`-separated, without empty, `.` or `..`
  components; every component a portable component, valid UTF-8; absolute,
  drive, UNC and verbatim forms rejected; ≤ 1024 bytes and ≤ 32 components;
- no two entries of one tree with equal paths after the S §2.1 collision key
  (case and Unicode canonical folding) per component; every parent of an
  entry is a `dir` entry listed earlier; `problem/problem.toml` present as a
  file;
- `kind`, `tree`, `class` from their vocabularies; files have `sha256`
  (64 lowercase hex); directories have size `"0"` and no hashes;
- the shared limits (§3.5): entries, per-file size, total size;
- duplicate members, non-canonical integers and the other R §3.5 rejections.

Restore then opens each payload file without following links (Unix
`O_NOFOLLOW`, Windows `FILE_FLAG_OPEN_REPARSE_POINT`), refuses anything that
is not a regular file, and checks its size and, while copying, its hash.
Links, special files and extra files in the snapshot directory are never
read; they are reported.

### 7.3 Transaction

| Step | Action | Crash point after it |
|---|---|---|
| 1 | Resolve and validate (§7.1, §7.2), lock-free | — |
| 2 | Workspace lock (with recovery, held to step 7). Destination `problems/<d>` with `d` = `--into` or the manifest's `dir`. Both predicates below are evaluated first. The problem: the S §2.1 resolution must be "create", or `problems/<d>` already restored, else `identity_conflict{entry}` (8). The history (only when it is restored, i.e. without `--into`): `.cpforge/fetch/<d>/` absent → to be restored; history already restored → nothing to do; anything else → `fetch_records_present` (8). No open transaction names `d`. Both already restored → `already_restored`, exit 0, nothing written | — |
| 3 | Create `.cpforge/archive/<txn>/` and publish `journal.toml` (`kind = "restore"`, `phase = "staging"`, `dir`, snapshot id and path, manifest hash, `history` = whether history is restored) | `restore.journal` |
| 4 | Unless already restored: stage `.cpforge/staging/<txn>/` from `problem/`: directories, then files copied and hashed (a mismatch → abort `snapshot_corrupt{path}`), executable bit applied on Unix (`0o777 & !umask`, else `0o666 & !umask`); the `--into` rewrite (§7.4) on the staged `problem.toml`; then **payload metadata validation** (below). When history is restored, copy `fetch/` into `<txn>/fetch/` the same way. Sync everything staged; replace the journal with `phase = "staged"`, the staged problem directory's object identity `problem_object` (or that of the already restored `problems/<d>`), and the history copy's object identity `history_object` | `restore.staged` |
| 5 | Unless already restored: containment (§7.5) immediately before publication, then `publish_dir_noclobber` of `staging/<txn>` to `problems/<d>`. **Commit point** | `restore.published` |
| 6 | If history is restored: containment of `.cpforge/fetch/`, then `publish_dir_noclobber` of `<txn>/fetch` to `.cpforge/fetch/<d>` | `restore.fetch_published` |
| 7 | Remove the journal, then the transaction directory | `restore.finished` |

- **Payload metadata validation** (before publication): the staged
  `problem.toml` must classify `valid` (S §9) with a supported schema and
  its `[problem] id` must equal the manifest's `identity` (before an
  `--into` rewrite) and, after any rewrite, be bound to `problems/<d>`
  (S §2.1); the staged `tests.toml` must be absent or `valid`; any failure
  aborts (`payload_invalid{record, reason}`, 8), nothing published. For a
  contest tree: `contest.toml` `valid`, its `[contest] id` (if present)
  equal to the manifest's identity, and its `[[problem]]` records equal to
  the manifest's members (identities and labels, in order); for an
  incomplete contest restored with `--partial`, the same comparisons
  against `intent.json` (contest identity, planned members in order), and
  each restored member's marker must name a member of the intent.
- **History already restored**: `.cpforge/fetch/<d>/` is a real directory
  containing every **history record** (`txn-*.toml`) of the snapshot's
  `fetch/` tree with its hash (history records are never rewritten, S
  §5.8.10); other entries only as the fetch contract allows: further
  `txn-*.toml` records whose `identity` is the problem's (later fetches),
  and a `status.toml` of any bytes or classification (it is replaced by
  every fetch attempt, S §5.8.9, so its snapshot copy is not compared). A
  `journal.toml`, a snapshot history record with other bytes or missing, a
  later record of another identity, or any other entry is a conflict.
- **Already restored**: `problems/<d>` is a directory bound to the target
  identity whose files include every payload file of the snapshot's
  `problem/` tree with its hash (`source_sha256` excepted: `problem.toml`
  must equal the snapshot's payload bytes, or, under `--into`, the
  rewrite of them). A rerun of `cpf restore` on such a destination
  restores only what is missing: the history (when `.cpforge/fetch/<d>/`
  is absent), and otherwise reports `already_restored` (exit 0).
- Submission versions are never restored (regenerable tool output,
  reported `submit_not_restored` when the snapshot has them).
- Output: the restored problem, omissions, and every external (§4.3) with
  `exists`: for `path` and `template` kinds whose `nsum` is not truncated,
  whether the full path exists now (a `symlink_metadata`, nothing opened
  or run); `null` (unknown) for programs and for truncated values, which
  cannot be resolved from a summary.
- Exit 0; 8 for every refusal and for an abort before the commit point; 9
  when the problem was published but step 6 failed (a rerun of `cpf
  restore` with the same snapshot then restores the history alone, above).

| Killed after | State | Recovery (rule R, §6.7) |
|---|---|---|
| step 3 | journal `staging`; staging may exist | staging removed by S §4.5 recovery first; then the transaction directory is removed (roll back) |
| step 4 | journal `staged`, staging complete | not published (`problems/<d>` does not hold `problem_object`): roll back as above |
| step 5 | problem published, history copy in `<txn>/fetch` | `problems/<d>` holds `problem_object` → continue: if `.cpforge/fetch/<d>` is absent and `<txn>/fetch` holds `history_object`, step 6; if `.cpforge/fetch/<d>` holds another object, the copy is removed (its bytes are in the snapshot) and `history_conflict` reported; then step 7. `problems/<d>` holding another object → the transaction directory is removed, nothing else touched, reported |
| step 6 | both published | `.cpforge/fetch/<d>` holds `history_object` → step 7 |
| step 7, journal removed | transaction directory without journal | initialization leftover: removed (rule R) |

### 7.4 `--into <dir>`

`<dir>` is an Identifier (S §2.1); the problem is restored as
`problems/<dir>` with identity `local:<dir>`. The staged `problem.toml` is
edited with the S §4.4 operation semantics: `[problem] id =
"local:<dir>"`; `[problem] origin` = the existing `origin` when present,
otherwise the snapshot's identity (a tool field, written only by restore);
every other byte unchanged. Catalog paths in `tests.toml` are relative to
the problem directory and stay as they are; `download` tables and `[fetch]`
stay as inert data. Saved stress failure ids are scoped by problem and stay
unchanged. Fetch history is not restored under `--into` (its identity would
differ from the problem's, S §5.8.10): reported
`fetch_history_not_restored`, the files it named become user content.
`--into` with a contest snapshot → 2.

### 7.5 Containment

Immediately before the publishing call of step 5 (and of step 6): every
component from the workspace root to `problems/` (and `.cpforge/fetch/`)
is checked with `symlink_metadata` to be a real directory (no symlink,
junction or other reparse point); the destination does not exist; its name
is checked by collision key against a fresh listing of the parent.
Publication is no-clobber (S §4.3), so an existing destination is never
replaced. As in S §1 and §2, these path checks give **no defence** against
a component swapped for a link between the final check and the rename;
that case is outside the guarantee and documented. Handle-relative
containment is a later design. A read-only `problems/` fails the
publication with the OS error; staging is removed.

### 7.6 Contest restore and `--partial`

`cpf restore c-…` restores the members and then the contest directory:

- Without the contest manifest the snapshot is `incomplete`: refused (8)
  unless `--partial`. With `--partial` the members that have a marker are
  restored and `contest/` last; `contest.toml` still lists every planned
  member, so the missing ones are dangling references (reported, as
  `cpf fsck` reports them).
- Each member snapshot is found at the path its marker or the manifest
  records, in the contest snapshot's root; its manifest hash must match.
- Preflight, before any publication: every member destination and
  `contests/<c>/` must resolve to "create", or be already restored (§7.3;
  a rerun after a crash skips such members, completing their history if
  needed). Any other state refuses the whole restore, listing every
  conflict.
- Each member is its own §7.3 transaction; then the contest directory is
  staged from `contest/` and published no-clobber as `contests/<c>`
  (a transaction with `kind = "restore"` and tree `contest`).

## 8. Listing and search across roots

`cpf ls --archived` and `cpf search` read archive roots with the S §5.9.9
reader rules (lock-free, no root lock, nothing written).

- **Roots**: `archive.roots` in configuration order, then each `--root`.
  A root that does not exist, is not a directory, or has no valid
  `root.toml` is reported (`root_unavailable`, `not_an_archive_root`,
  `root_record_<classification>`), never skipped silently. These two
  readers load configuration (layers 1 and 2) only to read `archive.roots`;
  a configuration error is reported as the finding `roots_unavailable` (the
  workspace part still runs, and `--root` values still apply), never exit
  7. `cpf ls` without `--archived` stays configuration-free.
- **Traversal** of one root: the no-follow walker from the root, skipping
  `.cpforge-archive`, to a depth of 9 directories (8 layout segments and the
  leaf). A directory whose name has the snapshot id grammar is a snapshot:
  its `summary.json` is opened (no-follow, regular, at most 64 KiB + 1
  read) and the presence of `manifest.json` is checked by one
  `symlink_metadata`; it is not descended into. A link is reported
  (`link`) and never followed; an unreadable directory is reported
  (`unreadable_directory`). These are restore's containment rules.
- **Bounds**: the S §5.9.8 counters E and B are shared by the workspace and
  every root of one invocation; each directory entry, each open attempt
  and each `symlink_metadata` counts one in E, each byte read counts in B.
  Exceeding either stops with `entry_limit` or `byte_limit` as in S §5.9.8.
  Roots are examined after the workspace.
- **Stalled roots**: no timeout. The S §5.9.8 cancellation already ends a
  search blocked in a system call (output within 200 ms of the first
  signal); `cpf ls --archived` installs the same listener and worker. The
  user documentation says a stalled network root needs Ctrl-C.
- **Results**: one per snapshot, `origin: snapshot`, with root, layout
  path, snapshot id, `complete` (manifest present; for a contest snapshot,
  its manifest), and the summary fields. A malformed or too-new summary, a
  missing manifest (`manifest_missing`) and an over-size summary are
  reported with the path.
- **`cpf ls --archived [filters]`**: snapshots only, in root order, then
  layout path and id in byte order; the S §5.9.6 filters apply to the
  summary fields (`--contest <c>` matches the summary `contests` by
  directory). `--archived --contests` lists contest snapshots with their
  member count and completeness.
- **`cpf search`** examines, after the workspace, every snapshot summary:
  the searched fields of S §5.9.8 from the summary (`identity`, `title`,
  `url`, `tags`, `status`), and `notes` from `problem/notes.md` (opened
  no-follow, first 64 KiB, the S §5.9.8 notes rules). `--no-archives`
  restricts it to the workspace.
- **`--archived-in <range>`** (shared filter grammar, S §5.9.6): matches
  `archived_at`, for active problems and snapshots alike.

## 9. Maintenance

- **Trees**: `.cpforge/archive/` (transactions) and `.cpforge/quarantine/`
  are reserved trees (S §3). Neither is disposable: `archive/<txn>/` is
  removed only by its transaction, its recovery or `--close`;
  `quarantine/<id>/` only by `cpf quarantine purge --apply`.
- **Leftover recovery**: `.cpforge/archive/<txn>/` is a metadata recovery
  location (temporary files of the journal replacement, S §4.5); rule R
  (§6.7); `.purge-*` deletion.
- **`cpf fsck`** reports: open transactions (kind, phase, lease state,
  `incomplete` items, a non-`valid` journal), quarantine entries by state,
  `.purge-*` leftovers, unexpected entries in `archive/` and `quarantine/`,
  `problems/<p>/` directories without `problem.toml` (already reported),
  and `[problem] origin` values that are not identities. `--apply` performs
  rule R (as every locked operation does) and nothing else for these trees:
  it never moves, trashes or deletes a retired or quarantined file.
- **`cpf status`** reports open archive transactions (`archive_incomplete`
  with the transaction id and the unsettled count) and, for each problem,
  nothing new.
- **`cpf clean`** never touches `archive/` or `quarantine/`.
- **`cpf gitignore`** suggests `.cpforge/archive/` and
  `.cpforge/quarantine/`: transaction state and displaced files are not
  project history.
- **Newer schemas**: a `too_new` journal, cleanup record, quarantine record,
  manifest or `root.toml` is never acted on, rewritten or removed; it is
  reported.
- **Records** (S §9): `journal.toml`, `root.toml` are tool-only TOML
  records (1 MiB cap); `manifest.json`, `summary.json`, `intent.json`,
  member markers, `plan.json`, `cleanup.json`, `record.json` are JSON documents of this
  contract's codec (§4.4), each with its own bound.

## 10. Tests

### 10.1 Harness

Every test drives the actual `cpf` binary on Linux, macOS and Windows
unless a row names an OS. `storage-test-hooks` provides: crash points (the
names of §5–§7, `<name>#i` counting from 1), pause barriers at the same
names, a clock override, a **trash double** (moves the first k items of the
bundle to a test directory, then returns an error, or moves all and returns
an error, or returns success without moving), a lease holder (an
independent child process holding a job lease), a sync-order trace, and a
bounds override for the shared limits. "Content asserted" means: every
file that existed in the problem before the archive is found, with its
bytes, in exactly the places the row names, by walking the workspace, the
quarantine, the root and the trash test directory; "no loss" means every
pre-archive file's bytes are found in at least one of them.

### 10.2 Plan

- **Classification table** on a fixture problem holding: `problem.toml`,
  `tests.toml`, `notes.md`, an unedited registered download pair, an edited
  download (answer changed), a superseded pair, a revision pair, a pair from
  a rolled-back fetch (`unreferenced`), a hand-written test, an unregistered
  `extra.in`, a file under `tests/` with a sample-like name and a hash no
  record names, `stress/failures/f-…/`, `main.cpp`, `alt.cpp`,
  `helpers.hpp`, `brute.py` (a configured language), `README.txt`: each
  file's class equals the table of §3.3; `helpers.hpp` (no configured
  extension) and `README.txt` are `other`, kept and listed by path.
- **Fetch history decides**: the same bytes under `tests/` are `tests:sample`
  with a valid history record naming them and `tests:custom` after the
  record is replaced by a `malformed` one.
- **Keep/discard**: default config keeps every class and omits `submit`;
  `discard = ["tests:sample"]` drops exactly the unedited download files;
  `--profile minimal` replaces the lists; `discard = ["metadata"]`,
  `discard = ["fetch"]`, a class in both lists, an unknown class → 7.
- **Layout vectors** (pure): `{site}/{problem_id}` → `atcoder/abc400_a`;
  values `CON`, `con.txt`, `a.`, `a `, `a/b`, `a\b`, `a:b`, `C:`, `\\x`,
  `.hidden`, a 256-byte value, a control character → refused before
  anything is written; `{label}` outside a contest archive →
  `layout_value_missing`; `{snapshot_id}`, `{tags}`, `{title}`, an empty
  segment, 9 segments → 7; an existing `Atcoder/` beside a value `atcoder`
  → `layout_collision`.
- **Payload names**: a file `a:b` (Unix), `CON.txt`, `x.` (Unix), two names
  `A.txt` and `a.txt` (Linux), a symlink, a FIFO (Unix), a junction
  (Windows), a 1 025-byte path, 33 components → each refused with the path
  listed, workspace and root byte-identical.
- **Limits**: inventories of 10 000 entries, 256 MiB for one file and 1 GiB
  in total (with the bounds override scaling sizes, and one unscaled 1 GiB
  run on Linux) plan successfully; 10 001 entries, 256 MiB + 1 and 1 GiB + 1
  → `snapshot_too_large` before any root write; 20 001 entries →
  `inventory_too_large`.
- **Binding**: the plan hash changes after each single change of: one byte
  of a kept file, one byte of a discarded file, a new file, an executable
  bit, `archive.layout`, `--profile`, `--cleanup`, the root, the selection,
  a contest listing the problem; `--apply --plan <old>` → 8
  `plan_changed`, nothing written. Two plans of an unchanged workspace have
  the same hash.
- **Integration**: a workspace after three refetches with edited and
  upstream-changed pairs, a restart and `cpf test register`, is classified
  from its fetch history: every downloaded file's class equals the
  expected table computed from the records.

### 10.3 Root guard, codec, snapshot

- **Lock order**: a root-lock holder never acquires the workspace lock
  (instrumented guard asserts on every acquisition); two processes
  archiving to one root serialise on the root lock; a held root lock gives
  `Busy` after the bound.
- **Root recovery**: a staging leftover is removed only after the root lock
  is acquired; with the lock held by a paused archive at
  `archive.copied#2`, a second process waiting for the lock never removes
  that staging; after the first is killed, the second removes it.
- **Codec vectors**: the worst-case manifest (10 000 entries, every field
  of §4.4 at its stated maximum) is encoded, its length recorded and
  asserted ≤ C, and read back; the largest entry the field limits allow
  encodes to at most 1 293 bytes (≤ the 1 536 budget), asserted by
  encoding it; a valid manifest padded with an unknown string member to exactly
  C bytes passes the size check and is read; a file of C + 1 bytes is
  refused before parsing; a writer fed a manifest above C (test-only
  oversized external) refuses to encode; duplicate members, `"01"` sizes,
  a JSON number in a `u64` field, uppercase hex → rejected. The same
  worst-case vectors for the summary (≤ 64 KiB, with `title_truncated`),
  `plan.json`, `cleanup.json`, `sweep-<n>.json`, `trash-result.json` and
  `record.json` at their item bounds.
- **Publication** (kill at each crash point of §5.2, then rerun):
  `archive.root_staged`, `archive.copied#1`, `#last`,
  `archive.manifest_written` → root has no snapshot, staging removed by the
  next root lock, workspace byte-identical; `archive.published` → one
  complete snapshot, workspace byte-identical. Disk full on the root
  (Linux: a small tmpfs root; macOS: a small disk image; Windows: a small
  VHD where CI allows, otherwise an injected `ERROR_DISK_FULL`) → 8,
  nothing published, staging removed.
- **Cross-filesystem root**: a root on another filesystem (Linux tmpfs,
  macOS disk image) archives successfully; the snapshot's files and hashes
  equal the source.
- **Overlap and links**: a root inside the workspace, equal to it, or an
  ancestor of it → `root_overlap`; a layout directory replaced by a link
  before the archive → `link_in_destination`; a concurrent creation of the
  layout path's last directory by another process between the check and
  `create_dir` → accepted when it is a directory, refused when it is a
  file.
- **Edit during copy**: at barrier `archive.copied#1`, an editor save of a
  file **not yet copied** → `source_changed` when it is copied, nothing
  published; a same-size save of the file **already copied** → the snapshot
  holds the bytes read, and cleanup later quarantines the edited file as
  `divergent` (content asserted); a new file created at the same barrier →
  `source_changed` at step 4.
- **Writer boundary**: snapshots at each shared limit are published, and
  their manifests read back by the reader of §4.4; `archived_at` in the
  snapshot's `problem.toml` equals `created_at`, every other byte equals the
  source, and `source_sha256` equals the source hash. **Transformed
  total**: with `archived_at` absent, a kept payload whose staged total is
  exactly 1 GiB (source total 1 GiB minus the added bytes, bounds override)
  is published and restores; a source total of exactly 1 GiB is refused
  `snapshot_too_large` before any root write; the same for the per-file
  limit with a 256 MiB `problem.toml` boundary under the bounds override.
- **Sync order** (Linux, macOS; trace): every snapshot file synced before
  the manifest is written, the manifest before the publication, the parent
  after it; the journal synced before commit point B. Windows: file flushes
  before each rename, no directory flush.
- **`reuse-identical`**: an unchanged problem archived twice → second run
  `reused`, no new snapshot; one changed byte, a changed executable bit, a
  changed `config.json`, or a corrupted payload byte in the existing
  snapshot → not equivalent → refused as `error`; `--on-existing new` → a
  second snapshot.

### 10.4 Retirement

- **Lease rule**: an independent process holding a job lease of another
  problem → every mode refused (`lease_held`), on Linux, macOS and Windows;
  an unreadable lease file (Unix permission, Windows deny ACL) → refused; a
  lease taken between the snapshot and retirement (barrier after
  `archive.published`) → `snapshot_only{lease_held}`, exit 9, problem
  unchanged.
- **`keep`**: the active problem, `state.toml`, every fact, every
  `contest.toml` and the fetch records are byte-identical (with file
  identities) after `--cleanup keep --apply`; one snapshot exists.
- **Kill at each step** of §5.3 (`archive.txn_created`,
  `archive.plan_published`, `archive.journal_published`,
  `archive.retired`, `archive.side_retired#1..3`,
  `archive.selection_cleared`, `archive.facts_removed`,
  `archive.phase_retired`), then any locked command (`cpf tag`): rolled
  back at `archive.txn_created`, `archive.plan_published` (transaction
  directory removed) and `archive.journal_published` (problem in place,
  selected, facts intact), otherwise rolled forward: problem gone from `problems/`,
  side trees in `<txn>/`, `current_problem` absent from `state.toml`, no
  facts of the problem, journal `retired`; content asserted.
- **Selection**: archiving the selected problem clears the selection and
  reports `selection_cleared`; archiving another problem leaves
  `state.toml` byte-identical; a `malformed` `state.toml` is left
  byte-identical and reported.
- **Contest member**: a problem listed by `contests/c/contest.toml` →
  `snapshot_only{member_of_active_contest}`, exit 9, the contest file and
  problem unchanged; a contest file that is `malformed` counts as listing.
- **Replacement after a crash**: kill at `archive.retired`, then create a
  new `problems/<p>` (by `cpf add` after recovery has run, and by hand
  before it) → recovery never touches the new directory; kill at
  `archive.journal_published`, then replace `problems/<p>` by hand with
  another directory → recovery leaves it, reports `source_missing`.
- **Windows busy directory**: a process whose current directory is inside
  the problem → `snapshot_only{retire_failed}`, problem unchanged.

### 10.5 Cleanup

- **Revalidation** in every mode (`trash` through the trash double,
  `quarantine`, `delete`): an editor save to a kept file and a new
  `helper2.hpp` written into the retired tree at barrier
  `archive.phase_retired` → both quarantined (`divergent`, `new`), the other
  files settled by the mode; under `delete` both still quarantined.
- **Late write through a pre-retirement handle**: a child opens `main.cpp`
  for writing before `--apply`, waits at the barrier between the cleanup
  hash and the move (`archive.before_unlink#i` under `delete`, the move of
  that item under `trash` and `quarantine`), writes 1 KiB, closes: the new
  bytes are present in the trash test directory or in quarantine; under
  `delete` they are gone and the plan output contained the fixed warning.
- **Discarded under `delete`**: `tests:sample` discarded with `--cleanup
  delete` → those files in quarantine with `delete_discarded`.
- **Kill between items**: kill at `archive.item_moved#i` for i = 1, 2 and
  the last, and at `archive.cleanup_planned`, `archive.trash_marked`,
  `archive.trash_called`, `archive.swept`, `archive.record_published`,
  `archive.teardown#1`; then `cpf archive --resume --apply`: the result is
  `complete` with every item in exactly one of {trash, quarantine, deleted,
  snapshot-only}, or `incomplete` with the exact list of the items left;
  a second `--resume --apply` advances it or reports the same list;
  content asserted, no loss in any cell.
- **More kill points**: `archive.sweep_planned#2`, `#3`, `archive.trash_result`,
  `archive.finishing`, each `archive.teardown#k`, `archive.closing`,
  `archive.close_moved#k`: after each, `cpf tag` (rule R) then `--resume
  --apply` reach the same terminal state as an uninterrupted run; a
  transaction directory left with `finishing` and no journal, or empty, is
  removed by rule R; an existing equal `record.json` is accepted, a
  hand-altered one stops with `record_mismatch` and nothing is removed.
- **Generations**: a process whose current directory is inside the retired
  tree recreates `main.cpp` right after its pass-1 quarantine move (barrier
  `archive.item_moved#i`), with and without a kill there: both
  generations end in quarantine (`files/1/…` and `files/2/…`), each with
  its own bytes; no move ever replaces an existing quarantine path
  (an injected existing destination → `destination_taken`, nothing
  replaced).
- **Unrecordable and oversized**: a file with a non-UTF-8 name (Unix) and
  a 2 000-byte path created in the retired tree → quarantined as one
  subtree item at their nearest recordable ancestor; 20 001 entries in the
  retired tree → each tree root quarantined as one subtree item; both
  records stay within their bounds.
- **Live record publication**: with cleanup paused inside the publication
  of `cleanup.json` (temporary file written, barrier before the rename), a
  concurrent `cpf tag` waits for the workspace lock and, once it gets it,
  finds no temporary file to remove; the publication completes.
- **Delete replacement**: under `delete`, at barrier
  `archive.before_unlink#i` and at a barrier just before the rename into
  `<txn>/delete/`, a process inside the retired tree replaces the file's
  name with a new file: the new file ends in quarantine with its bytes; the
  original is deleted only after its identity and hash were checked in
  `<txn>/delete/`. Kill after the rename and before the check
  (crash point after the rename): `--resume --apply` checks the captured
  replacement in `delete/`, quarantines it, never unlinks it. After every
  delete item is settled, `<txn>/delete/` is empty (kill at
  `archive.delete_pruned#i`, then resume: still empty at the end), and the
  teardown succeeds. **Redirect durability**: kill before
  `archive.redirect_recorded#i`, after it, and after `archive.redirected#i`,
  for a replacement captured at `delete/1/problem/src/main.cpp`: after
  `--resume --apply` the replacement is in `files/redirect/<i>/…` with its
  bytes, the original item is unsettled `replaced` (never `deleted`), and
  the transaction ends `incomplete` listing it; an explicit `--close
  --apply` then publishes `record.json` with the replacement as pass
  `redirect` and the original as unsettled, `delete/` holds no file or
  directory before `finishing`, and the transaction directory is gone.
  **Automatic nested teardown**: a same-object `divergent` redirect (the
  file at `delete/1/problem/src/main.cpp` rewritten in place at
  `archive.delete_moved#i`, after the rename and before the identity and
  hash check), with and without a kill after
  `archive.redirected#i`: the file is quarantined under
  `files/redirect/<i>/…`, the transaction ends `complete` and the
  directory is gone. **Unexpected staging file**: a file created by hand
  at `delete/1/problem/extra.txt` → left in place, `incomplete` with
  `unexpected_in_staging`; `--close --apply` moves it with `delete/` into
  `files/close/delete/…`.
- **`--close` cases**: a transaction with a malformed journal, one stuck in
  `retiring` with `source_missing`, and one with unsettled items, each
  closed with kills at `archive.closing`, each `archive.close_moved#k`,
  `archive.record_published`, `archive.finishing` and each
  `archive.teardown#k`: after `--resume --apply` (or `--close --apply`
  again) the transaction directory is gone, the closed subtrees are in
  `files/close/` with their bytes, and rule R never removed or rolled
  forward a transaction holding `closing`.
- **Quarantine record bound**: 19 994 pass-1 items (all `delete`) and
  2 × 4 096 sweep items with 1 024-byte paths, many `vanished`, one
  redirect per delete item, plus a `--close` → `record.json` encodes
  within 1 MiB + 56 390 × 1 600 and every item appears exactly once.
- **Held lease**: `--resume --apply` while the owner is paused at
  `archive.item_moved#1` → that transaction is skipped, untouched.
- **Fetch and submit trees**: matched records deleted; a history record
  edited by hand after retirement → quarantined.

### 10.6 Trash

- **Reconciliation** (trash double): all moved + error →
  `trash_error_after_move`, items trash; none moved + error → fallback to
  quarantine; first k of n moved + error → n − k quarantined, k matched
  items snapshot-only, a discarded item among the k → `incomplete` with
  `absent_after_trash`, then `--close --apply` → quarantine record lists it
  unsettled, transaction gone. Kill after `archive.trash_called` and after
  `archive.trash_result` in the partial case: recovery publishes (or keeps)
  the same result, and the discarded item stays unsettled; the transaction
  never reports `complete` while it is.
- **Unavailable** (Linux): bundle on a tmpfs with the home trash elsewhere,
  also with a user-owned `$topdir/.Trash-$uid` present whose `files/` is
  not writable → `trash_unavailable{cross_filesystem}`, items in
  quarantine, the crate never called (trace); a home trash on the same
  mount with an unwritable `files/` → `home_trash_unusable`; a home trash
  whose `Trash/` shares the bundle's mount but whose `Trash/files/` is a
  separate mount (tmpfs or bind mount) → `cross_filesystem`, crate never
  called; `HOME` and `XDG_DATA_HOME` unset → unavailable. In each case
  `cleanup.json` already records the items as quarantine
  `trash_unavailable`. Availability lost between revalidation and the call
  (the `files/` mount added at barrier `archive.trash_marked`): no backend
  call, `trash-result.json` all `fallback` before any move, kills at
  `archive.trash_result` and during the fallback moves → every item ends
  in quarantine.
- **Real backends**, each on its OS, with the workspace on a dedicated
  volume or temporary home so the trash is test-owned (Linux:
  `XDG_DATA_HOME` in the same tmpfs; macOS: an APFS disk image; Windows: a
  dedicated NTFS VHD or the system volume's Recycle Bin of the CI user):
  after a successful archive the bundle is in the trash with the object
  identity it had before the call and the late-write bytes of §10.5
  present; the macOS run sets `PATH` to an empty directory, so the Finder
  method (which starts `osascript`) would fail, and still finds the bundle
  in the volume's trash. Windows: a sharing violation on one bundle
  file → one retry, then fallback with every item in quarantine; a bundle
  path longer than 260 UTF-16 units → either recycled with content present
  or an error with the bundle intact, then fallback; Recycle Bin disabled
  for the test volume (`NukeOnDelete = 1`), and separately a maximum size
  below the bundle → the outcome (recycled, error with the bundle intact,
  or destroyed) is recorded in the test log and the user documentation;
  destroyed is the §1 exception 2 case of a user setting that disables the
  bin. Any observation the fallback rule of §6.5 names triggers it for that
  OS.

### 10.7 Quarantine

- `cpf quarantine ls` lists ready, pending (an open transaction) and
  `record_missing` entries and leaves the workspace byte-identical.
- **Retention boundaries** (clock override): `retention_days = 30`, an
  entry created exactly 30 × 86 400 000 ms before now → selected; 1 ms
  later → not selected; `retention_days = 0` → every ready entry;
  `created_at` in the future → not selected; a pending entry named by id →
  8; an entry with a malformed record → selected only by id.
- **Never automatic**: after 1 000 locked operations at a clock past
  retention, every entry is still present.
- **Kill points**: `purge.renamed`, `purge.deleting#1` → the next locked
  operation finishes the deletion; nothing outside the selected entries is
  touched.

### 10.8 Restore

- **Hostile manifests** (pure, each refused with its reason): absolute
  path, `C:\x`, `\\?\x`, `..`, `a/./b`, empty component, a component with
  `:` or a trailing dot, a reserved device name, `A.txt` and `a.txt`, `é`
  and `e`+U+0301, a file whose parent is not listed, duplicate member
  names, a size `"01"`, a size above 256 MiB, a total above 1 GiB, 10 001
  entries, a missing `sha256`, uppercase hex, a manifest of C + 1 bytes, a
  `snapshot` differing from the directory name, an identity whose `dir`
  differs, a newer schema.
- **Payload checks**: a payload file replaced by a symlink, a FIFO (Unix),
  a junction (Windows), a byte flipped, a truncated file → refused or
  aborted before publication, workspace byte-identical.
- **`--into` vectors**: a remote snapshot restored `--into b` →
  `id = "local:b"`, `origin` = the remote identity, every other byte of
  `problem.toml` unchanged, `tests.toml` byte-identical, stress failure
  directories unchanged, no `.cpforge/fetch/b/`, finding
  `fetch_history_not_restored`; restoring that problem's own snapshot
  `--into c` keeps the first `origin`.
- **Conflicts**: an existing `problems/<d>` (same identity, other
  identity, other spelling on Linux, a file) → refused with the entry;
  existing fetch records → refused; nothing written.
- **Containment**: `problems/` replaced by a link to another directory
  before the final check (barrier before step 5's check) → refused; the
  destination created by another process at the barrier → `Exists`, staging
  removed, nothing clobbered; a read-only `problems/` (Unix permission,
  Windows read-only ACL) → refused with the OS error. The swap after the
  final check is documented, not tested as a guarantee.
- **Payload metadata**: a correctly hashed payload whose `problem.toml`
  declares `local:b` under a manifest identity `local:a`, a `problem.toml`
  with `schema = 99`, malformed TOML, a `tests.toml` that is `malformed`,
  and a contest payload whose `[[problem]]` records differ from the
  manifest members → `payload_invalid`, nothing published.
- **Kill points** `restore.journal`, `restore.staged`, `restore.published`,
  `restore.fetch_published`, `restore.finished`, then any locked command:
  the §7.3 table; content asserted. At `restore.published`, a
  `.cpforge/fetch/<d>` created by hand before recovery → `history_conflict`,
  that directory untouched.
- **Unchanged fetch, then restore**: restore a remote snapshot; run an
  actual `cpf fetch` of that problem through the fetch harness with
  unchanged upstream data (outcome `unchanged`: `status.toml` replaced, no
  new history record); then `cpf restore` of the same snapshot →
  `already_restored`, exit 0, nothing written.
- **History rerun**: step 6 failing (injected) → exit 9; a rerun of the same
  `cpf restore` → history restored alone, problem untouched, exit 0; a
  further rerun → `already_restored`, 0, nothing written; with a later
  `txn-*.toml` and `status.toml` added by a fetch → still
  `already_restored`; with `status.toml` replaced (other bytes, or
  `malformed`) → still `already_restored`; with one snapshot `txn-*.toml`
  changed or removed, a later record of another identity, or a journal
  present → `fetch_records_present`, 8.
- **Externals**: a 2 000-byte template path → `value` truncated, `exists`
  `null` in the `restore` document.
- **`--partial` validation**: an incomplete contest whose `contest.toml`
  payload lists members other than `intent.json`'s → `payload_invalid`.
- **Externals and hooks**: a snapshot whose externals name a missing
  template and a program → both reported; an `on_switch` hook configured
  with hooks enabled → not run (trace: no process).
- **Round trip**: a snapshot made by cpf at each shared limit restores with
  every file, executable bit and hash equal to the snapshot's.

### 10.9 Contests

- **Kill mid-contest**: kill at `archive.contest_published`,
  `archive.member_marked#1`, `#2` of four members, and before
  `archive.contest_manifest`; `cpf ls --archived --contests` shows the
  contest snapshot `incomplete`; `cpf restore c-…` → 8, `--partial`
  restores the marked members and the contest directory with the dangling
  references reported; the rerun resumes the same contest snapshot and
  completes it with every member snapshot present; no member was retired
  before the manifest.
- **Members in two contests**: a member also listed by another contest →
  snapshotted, not retired, still active; the contest directory is retired
  (exit 9 for the member's `snapshot_only`).
- **Partial retirement**: kill at `archive.contest_run`, at
  `archive.member_retired#1` and `#2` of four, and at
  `archive.contest_retired`; then `cpf archive contest:<c> --apply` resumes
  the contest run (no `on_existing` refusal, no unresolvable-member
  refusal) and ends with every member and the contest directory retired;
  a member replaced by hand at `problems/<dir>` before the resume →
  `member_replaced`, left alone, contest directory kept.
- **Non-crash partial failure**: member A retired, member B's recheck fails
  (a lease taken at the barrier before B's retirement) → exit 9, contest
  run still open; after the lease is released, `cpf archive contest:<c>
  --apply` resumes the run (A `member_absent`, B retired, contest
  directory retired) and removes it.
- **Contest restore rerun**: kill after the second member's
  `restore.published` → the rerun skips the restored members and completes.

### 10.10 Listing and search

- `cpf ls --archived` over two roots: results in root, then path order,
  `origin: snapshot`, values from the frozen layout (a later tag change of
  a restored copy does not move or change the listed snapshot); a malformed
  summary, a missing manifest, an unreadable directory (Unix permission), a
  link inside a root, a missing root and a root without a marker are each
  reported with the path, never hidden behind "no matches".
- `cpf search` over the workspace and a root: hits from both, with origins;
  E and B are shared (an entry bound reached inside the root after the
  workspace hits → `entry_limit`, workspace hits kept); a stalled root
  (a FUSE or test filesystem that blocks `readdir`, Linux) → Ctrl-C prints
  the results so far within 200 ms; `--no-archives` reads no root
  (trace).
- `--archived-in` matches `archived_at` of snapshots and restored active
  problems; `ls --archived` before any root is configured lists nothing and
  says no roots are configured.

### 10.11 Maintenance

- `cpf fsck` reports each state of §9 on a fixture with an open
  transaction of each phase, a pending and a malformed quarantine entry, a
  `.purge-*` leftover and an unexpected entry; `--apply` changes only what
  rule R changes (byte-identical retired trees and quarantine).
- `cpf clean --apply` leaves `archive/` and `quarantine/` byte-identical.
- `too_new` journal, manifest, cleanup record, quarantine record and
  `root.toml` → never acted on, byte-identical, reported.

## 11. Amendments to other documents

### 11.1 Storage (`storage.md`)

Applied in this change, marked *(archive, draft)*: header note; §3 rows for
`.cpforge/archive/`, `.cpforge/quarantine/`, `[problem] archived_at` and
`origin`, the reserved-tree list and the archive exception to "never
deleted"; §4.3 directory publication sources; §4.5 recovery (metadata
location, rule R, `.purge-*`); §5.6 `fsck`; §5.8.10 records moving with
their problem; §5.9.6 `--archived-in`; §5.9.7 `ls --archived`; §5.9.8
archive roots; §9 records; §10 allowed-state rows and coverage.

### 11.2 Configuration (`config.md`)

§1.1 rows for `archive.*` and new §7.6 (the rules of §3.1), applied in this
change.

### 11.3 Report (`report.md`)

§5.1 code 9 wording, §5.5 rows, §6.1 availability and the `archive_plan`,
`archive` and `restore` documents, the snapshot members of `ls` and
`search`, applied in this change. The exit codes and the command sections
in `commands.md` follow with the command documentation.

### 11.4 Design and workspace design

Applied: workspace design §2.1 reserved trees. To be ratified in design
§7a and §11 with the other scope changes:

- archive refuses while **any** job lease of the workspace is held, not
  only jobs of the problem (§3.7);
- `keep` is a copy-only export decided before retirement, not a cleanup
  mode applied after it (§5.1); retirement renames the problem into
  `.cpforge/archive/<txn>/retired/` before cleanup revalidates it (§5.3,
  §6.2);
- `retention_days` applies only to an explicit `cpf quarantine purge`;
  nothing is purged automatically (§6.6);
- `delete` keeps design §7a's narrowed promise (§1 exception 1), and
  discarded files never go to `delete` (§3.4);
- conflict handling gains `on_existing = "new"` beside `error` and
  `reuse-identical` (§5.7);
- restore containment is path-based (§7.5).
