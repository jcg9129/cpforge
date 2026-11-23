# Storage contract

Status: v3. Normative for the `workspace` module and the storage side of
`jobs`. Initial scope: manual problems and tests, job directories, retention.
Fetch, generation-based publication and archives are planned for later
versions; the initial implementation reaches tests only through the
test-store interface (§8).

**Managed-workspace amendment (v5): settled, implementation in progress.** The
single-workspace rows of `../design-workspace.md` §9 cover admission sequence,
source capture, run facts, adopt / clean / fsck / gitignore transactions.
Amended content is marked *(draft)* below; init sequence bootstrap (C4c) and
local add with template provenance and contest membership (C4d) are implemented,
while the remaining amended transactions are planned.
References labelled *workspace design* point to `../design-workspace.md`.

**Fetch amendment: draft.** Remote identities (§2.1), the fetch transaction
and test publication (§5.8), the download area and fetch records (§3, §4.5),
catalog download fields (§8.1), their schemas (§9) and allowed states (§10).
Amended content is marked *(fetch, draft)*. It replaces generation-based
publication: tests are published through the catalog commit of §5.8.

Related: design §4, `execution.md` and `report.md` (report contract).

## 1. Assumptions and guarantee levels

- **Threat model:** cooperating local users and their ordinary tools
  (editors, compilers, the user's own programs); no adversary racing `cpf`.
- **Stable structural ancestors**: while an operation runs, the
  directories it walks through (workspace root, `.cpforge/`, `problems/`,
  `problems/<p>/`, `runs/`, a job directory) are not renamed, replaced or
  turned into links by anyone else. Ordinary edits and replacement of user
  *files* are expected and handled.
- **Supported filesystems**: ext4, xfs, btrfs, tmpfs (Linux), APFS
  (macOS), NTFS (Windows). Others are unverified; network and cloud-synced
  workspaces are outside every guarantee.
- **Three levels**: (1) *concurrent visibility*: readers see the old
  or the new complete file; (2) *`cpf` process crash*: guaranteed per §10;
  (3) *OS crash / power loss*: best effort (§4), never tested or promised.

## 2. Paths

- The workspace root is the nearest ancestor of the current directory
  containing a regular file `cpforge.toml` (`--workspace` overrides; a
  directory of that name is not a marker); it may be reached
  through a link and is canonicalised once. On Windows the canonical root is
  used in its plain drive or UNC form when it has one (not the verbatim `\\?\`
  form), so paths derived from it can be joined with `/` and passed to programs;
  a root whose plain spelling would name another file (a component ending in a
  dot or space, a reserved device name or a character Win32 rejects) stays verbatim.
- Tool-written paths come from the root and sanitised identifiers (portable-name validation). Each
  component below the root is checked with `symlink_metadata` at use; a
  symlink, junction or other reparse point is `LinkInPath`. Under §1 this
  suffices; it is no defence against component swapping.
- **Disposable trees** (job directories, `cwd/`, build directories) are
  assumed to contain **no mount points** (creating a mount visible to `cpf`
  requires privilege in its mount namespace; a mount the user creates there
  is outside the contract, stated in the user documentation).
  Measurement and deletion use one **own no-follow walker** (not
  `remove_dir_all`): it opens directories without following links
  (`O_NOFOLLOW | O_DIRECTORY` relative to the parent handle on Unix, reparse
  points not followed on Windows), removes links as entries, and stops at an
  entry whose device id differs from the tree root's or, on Linux, that
  `/proc/self/mountinfo` lists as a mount point (bind mounts on the same
  filesystem). Such an entry is skipped, left in place and reported; the
  containing directory is then not removed.
- Files the tool only reads may be links; test data must resolve to regular
  files (FIFOs, devices, sockets refused).
- Collision detection uses the portable-name key against existing names, under the
  workspace lock.
- **File-I/O names** declared by a problem (configuration contract) are one relative portable
  component: no separators, `..`, drive or root prefix, `:`, reserved device
  names, trailing dots or spaces.
- **Shared `cwd`** (configuration contract §4.1, any configured `cwd` other than the run's own)
  is a distinct capability: `cpf` never measures, clears or deletes
  anything in it; the record marks `shared_cwd`; file-based I/O problems with
  a shared `cwd` are rejected in the CLI foundation. In this version every
  file-I/O problem is rejected, regardless of `cwd` (configuration contract §6.2).

### 2.1 Portable names and identity

One module of pure functions (no filesystem access) defines a **lexical
policy**. It keeps names that are plainly unportable out of a workspace and
flags spelling collisions; it does not promise that every accepted name can
be created on every filesystem, nor that it finds every alias a filesystem
has. The filesystem itself is the final check: names are created
exclusively (§4.3), and a native refusal or collision is reported with the
native error, never treated as success.

- **Identifier**: problem names, contest ids, solution-target names, custom
  command names. 1–64 ASCII characters from `[A-Za-z0-9._-]`; first a
  letter or digit; last not `.`; no `..` substring; not a reserved device
  name (below). Used verbatim as a path component (`problems/<name>`); there
  is no sanitising step, an invalid name is an error naming the rule.
- **Portable component**: any other single component cpf creates or
  validates from user input (file-I/O names, source and test path
  components, `adopt` paths). Valid Unicode (`String`); 1–255 bytes in
  UTF-8, measured on the name as spelled (this also bounds UTF-16 length);
  not `.` or `..`; none of `/ \ : < > " | ? *`; no U+0000–U+001F (Windows)
  and no U+007F (cpf policy); not ending in `.` or U+0020; not a reserved
  device name.
- **Reserved device names** (Windows, applied on every platform):
  (a) **legacy devices** `CON`, `PRN`, `AUX`, `NUL`, `COM1`–`COM9`,
  `LPT1`–`LPT9`, and `COM`/`LPT` followed by superscript `¹`, `²` or `³`:
  matched ASCII case-insensitively against the part before the first `.`,
  after removing trailing U+0020 from that part, so `con`, `Nul.txt`,
  `CON .txt`, `nul.tar.gz`, `COM¹.txt` are reserved; (b) **console names**
  `CONIN$`, `CONOUT$`: matched ASCII case-insensitively, only as the whole
  component (so
  `CONIN$.txt` is accepted). Not reserved: `COM0`, `LPT0`, `COM10`,
  `COM4x`, `COM⁴`, and `CON` + U+00A0 + `.txt`.
- **Reserved prefix**: names starting with `.cpf-` (ASCII
  case-insensitive) are cpf's own temporary and staging names (§4.2,
  §4.5) and are rejected as identifiers and portable components.
- **Collision key**: Unicode canonical caseless matching,
  `NFD(CaseFold(NFD(name)))` with default full case folding; identifiers
  use the same key (for ASCII it equals ASCII lowercase). The Unicode
  version is the one of the pinned dependencies, recorded as a constant in
  the module and in `cpf doctor`; a dependency upgrade can flag a pair that
  was accepted before, which `fsck` reports and nothing renames. Two names
  with equal keys are a **spelling collision**. The key over-flags pairs a
  given filesystem distinguishes (acceptable: it only ever causes a
  refusal, never reuse of an entry) and may miss pairs a filesystem's own
  table equates (NTFS upcase tables, APFS repertoire changes); Windows 8.3
  short names (`LONGFI~1.TXT`) are not modelled. Those cases are left to
  exclusive creation on the current host.
- **Collision check**: under the workspace lock, a new name is compared
  with the entries of the target directory by key. An entry that is not
  valid Unicode (Unix bytes, Windows lone surrogates) is compared with the
  new name by exact native spelling only, without lossy conversion.
- **Path length**: no total-length check before use. cpf's own file
  operations use the platform's long-path support (Rust's standard library
  on Windows); an OS refusal for length maps to `PathTooLong{path}`.
  Compilers and solutions started by cpf have their own limits; their
  failures are reported as their errors.
- **Problem identity** (`problem.toml` `[problem] id`): a string
  `<site>:<native>`, split at the first `:`. In the CLI foundation `site`
  must be exactly `local` and `<native>` must be an Identifier (so `local:`,
  `local:a:b`, `Local:a`, `:a` are invalid identities); the general site
  grammar (1–32 characters from `[a-z0-9-]`, starting with a letter) and
  remote native syntax arrive with fetch. The identity is **bound to the
  directory**: `problems/<name>/problem.toml` must have `id =
  "local:<name>"` with the same spelling; any other value is
  `IdentityMismatch`, reported by `fsck` and `status`, and such a directory
  is not a problem for lookup. Remote sites arrive with fetch.
- *(fetch, draft)* **Remote identity**: `<site>:<native>` with `site` from
  the general grammar and not `local`, and `<native>` from that site's
  native-id grammar (sites contract). The directory is `<site>-<native>`,
  which must be an Identifier; an identity whose directory name is not one
  is refused by its adapter. `problems/<site>-<native>/problem.toml` must
  hold exactly that identity (same spelling); anything else is
  `IdentityMismatch`. The derivation need not be injective: a directory
  holds one identity, and a second identity deriving the same name (a
  local `atcoder-abc400_a`, or another site) is refused, never mapped to
  it. The resolution rules for `cpf add` apply to fetch creation with the
  derived name, rule 6 comparing with the remote identity. Contest
  directories are `contests/<site>-<native>/`, bound by `[contest] id`
  (§5.8.13); a local contest directory has no `[contest]` table.
- **Resolution for `cpf add <name>`** (§5.2 step 1), over the entries of
  `problems/` read under the lock. The first matching rule decides:
  1. No entry with the key of `<name>` → create.
  2. More than one such entry → `NameCollision{reason: SeveralEntries,
     entries}` listing all of them.
  3. The one entry is spelled differently (`Foo` for `foo`) →
     `DifferentSpelling`.
  4. The entry is a link (symlink, junction, other reparse point) →
     `LinkInPath` (§2); not a directory → `NotADirectory`.
  5. Classify its `problem.toml` (§9, user-editable records): `missing` →
     `MetadataMissing`, `unreadable` → `MetadataUnreadable`, `malformed` →
     `MetadataMalformed`, `too_new` → `MetadataTooNew` (nothing after the
     schema is decoded).
  6. Its valid `id` differs from `local:<name>` → `IdentityMismatch`.
  7. Otherwise → `Exists`.
  Each `NameCollision` names the entry (or entries) and the reason. A
  no-clobber publication that returns `Exists` although rule 1 applied is
  reclassified by these same rules: an exact directory with the same identity
  is `Exists`; other matching entries are refused. If no lexical match can
  be found (an alias the key missed), it is `NameCollision{reason: NativeExists}`.
- **Tests.** Pure unit tests:
  - every rule above with accepted and rejected examples;
  - reserved names: the accepted/rejected lists above, any case, trailing
    U+0020 before the dot, U+00A0 not trimmed;
  - lengths: 64/65-character identifiers; `é`×127 + `a` (255 bytes)
    accepted, `é`×128 (256 bytes) rejected; 127 × U+0390 (254 bytes)
    accepted although its key is longer;
  - keys: `ABC`/`abc`, `é`/`e`+U+0301, `ß`/`ss`, `ǅ`/`ǆ`, `ΐ`
    (U+03CA U+0301) / `Ϊ́` (U+03AA U+0301), combining marks in different
    canonical orders (`a` U+0301 U+0323 / `a` U+0323 U+0301 equal), marks of
    equal combining class keep their order (`a` U+0301 U+0300 / `a` U+0300
    U+0301 differ), ypogegrammeni (U+1F80 / U+1F00 U+0345 equal; U+03B1
    U+0345 U+0301 / U+03B1 U+0301 U+0345 equal, which needs the initial
    NFD); identifier `K` against the component `"\u212A"` (Kelvin sign)
    equal; `①` / `1` differ (no compatibility folding); key idempotence as
    a property test over generated strings;
  - portable components `.cpf-x`, `.CPF-tmp` rejected, `.cpfx` accepted
    (all three are invalid identifiers by the first-character rule);
  - identity parsing: `local:a` accepted; `Local:a`, `:a`, `local:`,
    `local:a:b` rejected;
  - every resolution outcome of `cpf add` (with an in-memory listing);
  - non-UTF-8 Unix entry and Windows lone-surrogate entry compared by
    native spelling.

  Integration tests: publishing a 255-byte destination component by
  atomic replace and by no-clobber publication; a workspace root whose path
  exceeds 260 UTF-16 units on Windows (cpf's own operations succeed);
  recovery removes an exact `.cpf-tmp-` name and keeps `.cpf-tmp-` + 31 hex,
  + 32 uppercase hex, and a directory of the exact name; recovery failures
  (an undeletable leftover, an unreadable location) are reported and the
  operation continues.

  Native probes on CI (one per OS, recording the filesystem): creating
  `a.txt` then `A.txt`, and `é` then `e`+U+0301, with exclusive creation;
  the test asserts that cpf's key flags both pairs and records whether the
  host filesystem refused the second name.

## 3. Ownership

| Path | Owner | How the tool writes it |
|---|---|---|
| `cpforge.toml` | user | created once by `cpf init`; never rewritten |
| `problems/<p>/problem.toml`, `tests.toml`, `contests/<c>/contest.toml` | shared | edit transaction (§4.4), only the command's fields |
| `problems/<p>/tests/*`, solution sources | user once created | created no-clobber; **never modified or deleted** |
| `problems/<p>/notes.md` | user | never written (CLI foundation) |
| `.cpforge/state.toml` | tool | atomic replace under the lock |
| `.cpforge/runs/<job>/` | tool, disposable | by its job (§6) |
| `.cpforge/staging/` | tool | only while holding the workspace lock |
| `.cpforge/lock` | tool | lock only; never unlinked or replaced |
| `.cpforge/job-seq` *(draft)* | tool | atomic replace under the workspace lock (§6.2) |
| `.cpforge/facts/<problem>/<target>.toml` *(draft)* | tool | atomic replace under the workspace lock (§6.5) |
| `problems/<p>/problem.toml` `[files] user` *(draft)* | user field | edit transaction by `cpf adopt` (§5.4) |
| `problems/<p>/problem.toml` `[files] created` *(draft)* | tool field | edit transaction by the creating command |
| `<root>/.gitignore` cpf block *(draft)* | tool block in a user file | only on `cpf gitignore --write` (§5.5) |
| `.cpforge/downloads/<txn>/` *(fetch, draft)* | tool, leased | created with its lease under the lock, written lock-free only by the lease owner; removed by the owner or by recovery when the lease is free (§5.8.3) |
| `.cpforge/fetch/<p>/journal.toml` *(fetch, draft)* | tool | no-clobber under the lock; removed when its transaction finishes or is reconciled (§5.8.7) |
| `.cpforge/fetch/<p>/txn-<txn>.toml` *(fetch, draft)* | tool, durable | no-clobber under the lock; never rewritten or removed by cpf (§5.8.10) |
| `.cpforge/fetch/<p>/status.toml` *(fetch, draft)* | tool | atomic replace under the lock (§5.8.9) |
| `problems/<p>/tests.toml` `[fetch]`, entry `download`; `problem.toml` `[problem]` `url`, `title`, `kind`, `limits`; `contest.toml` `[contest]` *(fetch, draft)* | tool fields | edit transaction by `cpf fetch` (§5.8), never another command |

Tool fields (`id`, `url`, `kind`, published limits, `schema`) and user fields
(tags, status, language, comparator, comments, unknown keys) share a TOML
file. A command changes only its own fields; untouched items survive as a
`toml_edit` round-trip. Permanent user files (sources, tests, notes) are
outside every retention budget.

**Reserved trees** *(draft)*: `.cpforge/runs/`, `.cpforge/cache/`,
`.cpforge/staging/`, `.cpforge/facts/`, *(fetch, draft)* `.cpforge/downloads/`,
`.cpforge/fetch/` and the files `.cpforge/state.toml`,
`lock`, `job-seq` belong to cpf in their entirety, whoever created an entry.
Only `runs/` (by §6.3 pruning and the job's own §6.4 cleanup) and `cache/`
(initial feature release) are disposable; a `downloads/` entry is removed
only by its protocol (§5.8.3), and `fetch/` is not disposable. No path under `problems/` or `contests/` is
disposable; outside the reserved trees nothing is deleted automatically,
with one exception: leftover recovery (§4.5) removes regular files named
exactly `.cpf-tmp-` + 32 lowercase hex digits in its metadata recovery
locations (a reserved name, §2.1). Fetch never deletes or modifies a file
under `problems/<p>/tests/` (§5.8).
Provenance and visibility of problem files are defined in workspace design §2.2.

## 4. Primitives

### 4.1 Publication result

Every publishing primitive returns

```rust
pub struct Published { pub committed: bool, pub durability: Durability, pub cleanup: Cleanup }
pub enum Durability { Ok, Uncertain(io::Error) }     // a flush or directory sync failed after commit
pub enum Cleanup { Done, Pending(PathBuf) }          // a temp name could not be removed
```

or an error with `committed = false` semantics (target unchanged; a temp file
whose removal failed is reported as `Cleanup::Pending` in the error). Callers
**continue their transaction after `committed`** and report the other two
fields as warnings; a committed publication is never reported as a
retryable failure.

**Metadata:** Unix mode of a replaced file is copied to the temp file.
Ownership, ACLs, extended attributes, Windows security descriptors and Windows
file attributes (read-only, hidden, …) are **not** preserved: the new file gets
what its directory gives new files. A read-only target on Windows makes the
replace fail with the old file intact (reported). Documented for users.

### 4.2 Atomic replace (files)

1. Create `.cpf-tmp-<random>` in the target's directory, `<random>` 32
   lowercase hex digits (128 random bits), independent of the target name so
   its length is bounded (`create_new`; an existing name is retried with new
   bits), write, `sync_all` (macOS also `F_FULLFSYNC`, best effort;
   Windows `FlushFileBuffers`). For replacement, apply the target's Unix
   mode before writing bytes, then restore it after writing (which may clear
   special bits) and sync it with the content. New files use the default
   `0666 & !umask` mode.
2. Unix `rename`; Windows handle-based rename with `FileRenameInfoEx`
   (`FILE_RENAME_FLAG_REPLACE_IF_EXISTS | FILE_RENAME_FLAG_POSIX_SEMANTICS`,
   falling back to `FileRenameInfo` before Windows 10 1607), as Rust's
   `std::fs::rename` does, so a destination open with delete sharing is
   replaced; never a copy.
3. Windows sharing violation / access denied: retry with backoff, monotonic,
   ≤ 2 s total, revalidating before each attempt for edit transactions
   (§4.4); then fail, old file untouched, temp removed. Never
   delete-then-rename, never truncate in place.
4. Unix: `fsync` the parent directory (failure → `Durability::Uncertain`).

### 4.3 No-clobber publication

- **Files** (`publish_file_noclobber`): temp as §4.2 step 1, then Linux
  `renameat2(RENAME_NOREPLACE)` (filesystem refuses it or `ENOSYS` → `link` + `unlink`,
  a failed unlink → `Cleanup::Pending`), macOS `renamex_np(RENAME_EXCL)`,
  Windows `MoveFileExW` without replace. Existing target → `Exists`.
- **Directories** (`publish_dir_noclobber`): Linux `renameat2(RENAME_NOREPLACE)`
  and macOS `renamex_np(RENAME_EXCL)` are **required** (no fallback:
  `Unsupported{op, filesystem}`); Windows `MoveFileExW` without replace or
  copy flags. The actual result decides: `EXDEV` / `ERROR_NOT_SAME_DEVICE`
  → `CrossFilesystem` (a device-id precheck is only a hint). Any failed
  directory publication leaves staging intact and reports `Cleanup::Pending`;
  the next outer operation's recovery removes it (§4.5).
- **Windows retries**: file and directory no-clobber publication use the same
  monotonic backoff as replacement (≤ 2 s total) for sharing violations and
  access denied, without edit revalidation. On failure, a file temp is removed
  (or reported as `Cleanup::Pending` if removal fails); directory staging is
  retained as above.
- **Pre-commit vs post-commit**: a failure before the publishing
  call (temp write, `sync_all`, staged-directory sync) is an error with
  `committed = false`, nothing published; a flush or sync failure after it is
  `Durability::Uncertain`. Before a directory publish: staged files
  `sync_all`, staged directories synced bottom-up. After any publish (file or
  directory), Unix syncs the target's parent, the source parent when it
  differs, and every ancestor directory the operation newly created **together with
  that ancestor's own parent**, bottom-up (best effort,
  `Durability::Uncertain` on failure). Only newly created ancestors of this
  publication's source and target participate; synced ancestors are retired
  from tracking and removed directories are discarded.

### 4.4 Edit transaction (shared TOML files)

Runs with a borrowed workspace guard (§4.5).

1. From one no-follow read through the guard's checked paths, record the
   target's bytes, length, SHA-256, file identity (Unix device + inode;
   Windows volume serial + file index), file type and hard-link count.
   Refuse a link, a non-regular file, or more than one hard link.
2. Parse (`toml_edit::DocumentMut`), run the caller's schema check (§9)
   **before** applying the command's **semantic operations** to this freshly
   read document. Untouched items, comments and formatting round-trip
   byte-for-byte.
3. If the operation leaves the document unchanged, validate the baseline
   once more and return without rewriting the file (`committed = false`,
   no durability or cleanup warning); a changed baseline is `Conflict`.
   Otherwise write the temp file (§4.2 step 1).
4. **Validate before each publish attempt** (also every Windows retry):
   re-read and recheck bytes, length, SHA-256, identity, file type **and
   hard-link count**; any difference, disappearance or type change → temp
   removed (or cleanup warning), `Conflict`. The primitive returns Conflict
   without replay. The caller may explicitly opt into a replay wrapper:
   at most two attempts total, each starting from 1 and rechecking the schema.
   Both `check` and operations are replayable `Fn` callbacks with no external
   side effects. A second Conflict is returned as Conflict. Cleanup warnings
   from the first attempt survive in the replay result.
5. Publish (§4.2 steps 2–4).

**Guarantee, stated exactly:** *The tool aborts when the final validation
observes bytes different from its baseline. This is not compare-and-swap;
concurrent edits during validation or before successful replacement may be
overwritten.* Between cooperating `cpf` processes there is no lost update.
An editor saving an older buffer after the tool overwrites the tool's change.

### 4.5 Locks

- **Workspace lock:** process-local mutex, then an exclusive OS lock on
  `.cpforge/lock`, cached per process, non-inheritable. After locking, compare
  the handle's identity with the current no-follow directory entry; if replaced,
  drop the cached handle and retry within the same acquisition bound. Unix `flock`;
  Windows `LockFileEx` on byte range `[0, 1)` of that dedicated file (a
  mandatory range lock; the file is never read or written). Non-blocking
  attempts with monotonic backoff up to 10 s, then `Busy{op}`.
- **Composition:** each public operation acquires the lock once and passes
  `&WorkspaceGuard` to its internal steps (edit transactions, publications,
  staging); internal steps require the guard and never acquire. The guard is
  not reentrant.
- **Leftover recovery** runs once at outer-operation entry, right after
  acquiring the lock and before the operation creates anything: staging
  directories and temporary files are only ever created under the lock, so
  any found then belong to a dead or finished operation and are removed.
  The scan is exact, not a glob: every entry of `.cpforge/staging/`
  (removed with the no-follow walker, §2, whatever its type), and in the
  **metadata recovery locations** (the workspace root, `.cpforge/`, each
  `problems/<p>/` and `problems/<p>/tests/`, each `contests/<c>/`, and
  *(draft)* each `.cpforge/facts/<p>/`), non-recursively, only regular files
  named exactly `.cpf-tmp-` + 32 lowercase hex digits. The `.cpf-` prefix is
  reserved (§2.1), so no name cpf validates can match; a matching file
  created outside cpf is removed (documented, §3). Temporary files inside a
  job directory (e.g. from publishing `job.toml`, §6.2) are not recovered
  here: they go with the job directory when it is pruned (§6.3).
  *(fetch, draft)* Each `.cpforge/fetch/<p>/` is a metadata recovery
  location too. Recovery also reclaims download areas whose lease is free
  and `.reclaim-<txn>` leftovers in `.cpforge/downloads/` (§5.8.3); a held
  or unprobeable area is kept. Every locked operation does this, `cpf
  clean` plan mode included. Recovery never reconciles fetch journals
  (§5.8.7).
- **Recovery outcome**: a missing location is skipped; a location that
  cannot be enumerated, or a leftover that cannot be removed, is kept and
  reported as the warning `RecoveryIncomplete{paths, errors}` of the
  operation, which continues (temporary files use exclusive creation,
  retrying with new random bits if the chosen name already exists, §4.2).
  Recovery never fails an operation by itself.
- Held only around metadata mutations, staging, job admission and pruning
  decisions; never while building, running, checking, during hooks or
  commands.
- **Leases** (§6.2) are separate lock files. A job takes its lease under the
  workspace lock and keeps it; it may take the workspace lock later.
  Pruning probes leases **non-blockingly** with **independently opened**
  probe handles, so the orders cannot deadlock and a probe never clones an
  owner's handle. Windows: lease and owned handles are opened with
  `FILE_SHARE_DELETE` so a job directory can be renamed while a lease file in
  it is open.

## 5. Command transactions

### 5.1 `cpf init`

Targets an existing directory (default: current directory); never creates the
root. Creates missing `.cpforge/`, `.cpforge/runs/`, `.cpforge/staging/` and
the lock file (refusing links) even when `cpforge.toml` exists; then creates
`cpforge.toml` no-clobber if missing. An existing `cpforge.toml` is never changed.

If `cpforge.toml` was **absent when checked under the lock**, it creates
`.cpforge/job-seq` (fresh random 128-bit epoch, `next = "0"`) no-clobber
**before** publishing `cpforge.toml`; an existing `job-seq` (from an
interrupted initial `init`) is classified under §9 and kept byte-for-byte
only if valid. An unreadable, malformed or too-new sequence is reported by
classification and path; init fails without changing it or publishing
`cpforge.toml`. A no-clobber collision on `job-seq` is reclassified in the
same way; a collision on `cpforge.toml` reports already initialised.
If the sequence disappears before reclassification after a collision, init
refuses rather than retrying a changing record. A rerun **on an existing
workspace** never creates or repairs `job-seq`; recovery is only by `cpf fsck --apply`
(§5.6). When the root is inside a git work tree, `init` mentions `cpf
gitignore` and writes nothing else.

### 5.2 `cpf add <problem>`

1. Lock. Check identity and portable name; same identity existing →
   `Exists` (with `--contest`: verify its `problem.toml` identity, go to 4).
2. Stage `.cpforge/staging/<txid>/` (problem files, solution from template).
3. `publish_dir_noclobber` to `problems/<name>`; not committed → staging
   removed, error.
4. Contest membership: edit transaction on `contest.toml` (created no-clobber
   when new), entry added only if absent.
5. Step 4 failing after 3 committed → "problem created; contest membership
   failed: <reason>", failure class `partial` (exit 9, report contract
   §5.1); rerunning `cpf add --contest` completes it without touching problem files.

`contest.toml` schema 1 uses an ordered array of records (`[[problem]]`),
with a string `id` (a problem identity, §2.1) and optional string `label`
(1–16 ASCII letters, digits, `-`, `_`). Position in the file defines order:

```toml
schema = 1

[[problem]]
id = "local:two-sum"
label = "A"
```

Unknown keys in each record, including future contest overrides such as
`limits`, are preserved untouched. An existing `id` is a no-op; an explicitly
supplied `--label` differing from its stored label (including a stored absent
label) is a conflict. Omitting `--label` keeps any existing label. A label
already used by another record is a conflict. Duplicate ids or labels and invalid record
fields are `malformed`; malformed or newer-schema files are refused without
mutation. Labels compare by exact spelling. New contest files are created
no-clobber with `schema = 1` before the replayable membership edit.

Before staging, `add` loads configuration with the prospective record's
`[config]` as the problem layer (configuration contract §6.5): for local
`add` only an explicit `--language`. Only an
explicit `--language` is recorded under `[config]`; the default remains
inherited. An unresolved language or an absent template creates no solution.
A configured template requires exactly one effective solution file
(configuration contract §6.5; by default the sources), and must
resolve to a regular file; it lies outside the workspace and is only read,
so links are followed (§2). Validation and reading finish before
publication. Every component of the solution destination must be a portable
component (§2.1) and a single-component destination must not collide with
`problem.toml`; otherwise `add` fails before staging. A copied solution is recorded as
`[files] created = [{ path = "main.cpp", command = "add" }]` in the staged
record, with the effective source path in place of `main.cpp`.

### 5.3 Test commands

The problem is resolved per §5.7. The catalog is §8.1.

**`cpf test add --input <file> [--answer <file>]`.** `-` reads one of the
two from stdin (both from stdin is a usage error); omitting `--answer`
registers a test without a reference answer. Bytes are copied exactly, never
linked or converted; test data has no size cap and is streamed. Explicitly
named sources may be non-regular files, including FIFOs: they are read as
streams (unlike registered test data, which must resolve to regular files).
Both sources are read first, **before** the lock is taken (a slow stdin never
holds it), into private temporary files in the operating system's temporary
directory, never in a workspace recovery location: §4.5 recovery assumes
every workspace temporary was created under the lock and would remove one
created before it. These captures are removed when the command ends; a
crash can leave them for the operating system to clean. Under the lock the
bytes are published from them with the §4.3 primitive. Then:

1. Lock (leftover recovery first). Classify `tests.toml` (§8.1): `missing` or
   `valid` continue; anything else refuses before anything is published.
   Choose the smallest `n ≥ 1` such that no catalog id has the collision key
   of `custom-<n>`, no catalog `input` or `answer` path has the key of
   `tests/custom-<n>.in` or `tests/custom-<n>.ans` (even when that file is
   missing: a registered path is reserved), and no entry of `tests/` has the
   key of `custom-<n>.in` or `custom-<n>.ans` (any entry type: a directory or
   link occupies a name).
2. `publish_file_noclobber` the `.in`, then the `.ans` if any.
3. Edit transaction on `tests.toml` (created no-clobber with `schema = 1`
   when missing) appending the entry `{ id = "custom-<n>", input, answer?,
   origin = "custom", enabled = true }`. A replay revalidates the fresh
   document and reapplies only this insertion; it never republishes files
   or chooses another `n`. The id and path availability is checked again
   inside the insertion on every replay; if a concurrent edit took the id or
   a path, the edit fails with a conflict (the files stay, step 4).
4. **Published files are never deleted.** If the `.ans` publish fails
   (`Exists` from a concurrent creator, I/O error) or step 3 fails, the
   published files stay and the command reports exactly which paths were
   published, that registration did not commit, the failing step and any
   warnings; `ls` shows them as unregistered, and the command exits with
   failure class `partial` (exit 9, report contract §5.1). A retry may choose
   a new `n`. A file edited by the user between publication and registration is
   registered as it is (no hash pins its content). `Durability::Uncertain`
   after a publish means it happened: the sequence continues with a warning
   and does not by itself make the result partial.

Output: the new id on stdout.

**`cpf test register <input> [--answer <file> | --no-answer] [--id <id>]`.**
Registers existing files without copying or changing them. `<input>` and
`--answer` name files in the problem's `tests/` (a path relative to the
current directory, which must resolve lexically to `tests/<name>` of the
selected problem). Without `--answer` or `--no-answer`, the sibling
`<stem>.ans` is used **if it exists**, and the output says so; `--no-answer`
ignores it. The id defaults to the input's stem when that is a valid
Identifier (§2.1) not taken in the catalog, else `--id` is required. Under
the lock: classify the catalog, check the files (§8.1 health rules), refuse
paths already named by an entry, then one edit transaction appending `{ id,
input, answer?, origin = "custom", enabled = true }`. As for `add`, the id
and path availability is checked again **inside** the insertion on every
replay, against the fresh document; if an editor took either meanwhile, the
command fails with a conflict and the catalog is unchanged.

**`cpf test enable <id>` / `cpf test disable <id>`.** Registered ids only (an
unregistered file must be registered first). Under the lock, an edit
transaction that changes only that entry's `enabled`, located by id on every
replay (never by position); an id gone during replay is reported, nothing
else changes. Already in the requested state → nothing is written.

**`cpf test ls`.** Lock-free reader (§8). One line per registered entry in
catalog order (id, enabled, answer absent / present, health), then the
unregistered candidates (§8.1) in collision-key order, then diagnostics.
Human-readable only; a stable machine format comes with `--json`.

A bare `cpf test` runs every enabled registered test in catalog order.
Repeatable `--case <id>` selects exactly those registered ids in option order,
including disabled entries; duplicate ids are a usage error. Unknown ids and
unregistered candidates refuse before admission. Case selection never uses a
positional id, so it cannot collide with these subcommands. Zero selected tests
still build the target, then report `no_tests` if the build succeeds.

### 5.4 `cpf adopt <path>` *(draft)*

Lock. Validate the path (relative to the problem directory, no `..`, no link
component, an existing regular file, not in a reserved tree); edit
transaction on `problem.toml` adding it to `[files] user` if absent.

**`[files] created`** (workspace design §2.2 as amended): only files a
command creates that no other record names. In the CLI foundation that is the template solution of `cpf add`, whose
entry is written into the **staged** `problem.toml` (§5.2 step 2), so it is
published atomically with the problem directory: no partial state. Tests
added by `cpf test add` need no entry: their `tests.toml` registration
already gives provenance `registered` (precedence, workspace design §2.2), and a test
published but not registered (§5.3 step 4) has provenance `unknown` and is
kept. **Both lists** are validated like `adopt` paths when read; invalid or
hand-edited entries that fail validation are reported by `status`/`fsck`
and ignored, never acted on.

### 5.5 `cpf gitignore [--write]` *(draft)*

Prints exactly these lines: `.cpforge/runs/`, `.cpforge/cache/`,
`.cpforge/staging/`, *(fetch, draft)* `.cpforge/downloads/`, then the comment `# optional: .cpforge/facts/`
and *(fetch, draft)* the comment `# optional: .cpforge/fetch/` (fetch history
is the provenance of downloaded test files; ignoring it makes them user
content in another checkout, which is safe).
`--write` is a **text edit transaction**, §4.4 with these differences:
lock; read `<root>/.gitignore` (absent → publish no-clobber a file holding
only the block, §4.3); refuse a link, a non-regular file or more than one
hard link; locate the block: exactly one line `# >>> cpforge` and one later
line `# <<< cpforge` → replace the lines between them; no marker → append
the block (preceded by a newline if the file does not end with one); any
other marker arrangement (one marker, duplicates, reversed) → refuse,
nothing written. Validate before each publish attempt (also every Windows
retry) exactly as §4.4 step 4, then atomic replace (§4.2). Same stated
guarantee as §4.4: validation is not compare-and-swap.

### 5.6 `cpf fsck [--apply]` *(draft)*

Lock (with the usual §4.5 leftover recovery, as for every locked
operation). Reports: dangling contest references, problem directories
without `problem.toml`, schema problems (§9), provenance overlaps and invalid
`[files]` entries (workspace design §2.2), facts for deleted problems, facts for unknown
targets, unexpected entries in reserved trees, and the state of `job-seq`
(§9 record classification).

`--apply` repairs **only cpf records**:

- dangling contest references: removed by edit transaction;
- facts of **deleted problems**: fact files removed, but only those
  classified (§9) `valid` or `malformed`; a `too_new` fact is reported
  and kept (no mutation), and an `unreadable` one is reported and kept. Facts for unknown targets are reported only;
- `job-seq` `missing` or `malformed`: **only if no job may be live**: every
  directory in `runs/` (including newer-schema and `.reclaim-*` ones) is
  probed as in §6.3 step 2; a held lease **or a lease that cannot be
  opened** → refuse and list those jobs. Otherwise publish `{ schema, fresh
  epoch, next = 0 }` by atomic replace (§4.2). `too_new` is never
  repaired (§9: no mutation); the user needs a newer cpf. `unreadable` is
  never repaired either (it may be a transient access problem): reported
  with the I/O error; the user fixes access, or removes the file, after
  which it is `missing`.

*(fetch, draft)* `fsck` also reports: fetch journals (`fetch_incomplete`,
§5.8.12) and their classification; held and unprobeable download areas;
unexpected entries in `downloads/` and `fetch/`; non-`valid` history and
status records; history records whose identity differs from the
problem's; fetch records of problems that no longer exist (reported only,
never removed: history is provenance). `--apply` reconciles every `valid`
journal (§5.8.7) and removes `malformed` journals; it never removes a
download area other than through recovery's lease probe, never a history
or status record, never a test file.

Never user files, never `.cpforge/lock`, never lock abandonment inferred
from age or PIDs.

### 5.7 Selection: `cpf current`, `cpf switch`

A command that **needs a problem** resolves it once, at start, with the first
source that is present; precedence picks a source, it never searches for a
candidate that happens to be valid:

1. `--problem <ref>` (global flag).
2. The **cwd problem**: the current directory is `<root>/problems/<x>` or
   below it. Ancestry is **physical**: the current directory as the
   operating system reports it, canonicalised once, compared component-wise
   with the canonical root (§2). A link is never part of that path, so
   entering `problems/a` through a link to `problems/b` selects `b`, and a
   link that leads outside `problems/` gives no cwd problem. `<x>` is then
   validated like a reference (below), collision check included.
3. The **saved selection** in `.cpforge/state.toml`.

Only the winning source is read: a valid `--problem` or cwd problem succeeds
even when `state.toml` is broken, and the cwd problem is not inspected when
`--problem` is given. An error in the winning source never falls through to
the next one:

| Situation | Result |
|---|---|
| `--problem` cannot be resolved | error; no fallback |
| current directory not below `problems/<x>` | no cwd problem; the saved selection is consulted |
| cwd problem `<x>` is not a valid problem (invalid identifier, link, not a directory, `problem.toml` not `valid`, `IdentityMismatch`) | error naming `<x>` and the cause; no fallback |
| `state.toml` missing, or valid without `current_problem` | `NoSelection` |
| saved identity names a missing or invalid problem | error naming the saved identity and the cause |
| `state.toml` `unreadable`, `malformed` or `too_new` and the saved selection is needed | error |
| outside a workspace | workspace-required error |

**References** (`--problem`, `cpf switch`): `<name>` or `local:<name>`, where
`<name>` is an Identifier (§2.1). A reference, the cwd problem's `<x>` and a
saved identity all resolve through the same lookup, the rules of §2.1 for an
existing entry of `problems/`: exactly one entry with the key,
spelled exactly as given, a directory and not a link, `problem.toml`
`valid`, identity bound to the directory. A different spelling, several
entries, a link or a mismatched identity is an error, never corrected or
chosen, so with `problems/A` and `problems/a` both present (Linux), neither
is selectable by any source. `<contest>/<label>` and path references are
later designs. **Readers** (`current`, and every command that only resolves
its problem) perform this lookup **without** the workspace lock, listing
`problems/` and classifying `problem.toml` directly; the §2.1 collision check
under the lock is required only where a name is created. A concurrent
change can make a reader's answer stale, never a mixture of two states of
one file. `switch` performs the same lookup under the lock.

Selection needs a problem, not a target: it works for every problem kind,
including unknown kinds, and needs no language, sources or template.
Configuration is then loaded for the **resolved** problem (configuration
contract §1.2), not for the cwd problem when another source won.

**`state.toml`** (tool-only, §3, §9): `schema = 1` and an optional
`current_problem` (a valid identity, §2.1); unknown keys are `malformed`.
Schema 1 is owned entirely by its writer: a field added later needs a new
schema version. Classified with the tool-only record classification (§9).

**`cpf current`** prints the identity of the problem this invocation would
use (the precedence above), followed by a newline, and nothing else;
`cpf current --path` prints its absolute directory instead (native bytes on
Unix, the platform's string form on Windows, where the verbatim `\\?\`
prefix of a drive or UNC path is removed so shells can use it), followed by
a newline. A path
that contains a newline is printed as is; line-oriented consumers are the
target. On failure stdout is empty, the diagnostic goes to stderr, and the
command fails with failure class `workspace` (exit 8, `NoSelection`
included; report contract §5.1). `current` is read-only: no lock, no recovery, no write, no hooks.

**`cpf switch <ref>`** changes only the saved selection; it does not
change what the current directory selects. Under the workspace lock, after
leftover recovery (§4.5): resolve `<ref>` (above), classify `state.toml`;
`missing` or `valid` → if `current_problem` already equals the identity,
nothing is written (still a successful switch); otherwise atomically replace
(§4.2) with `schema = 1`, `current_problem = "<identity>"`. A valid state
naming a stale problem is replaced without resolving the old value.
`unreadable`, `malformed` or `too_new` → refused, nothing written (the
user removes or repairs the file; a recovery command is a later design).
`--problem` together with `switch` is a usage error. Output: exactly
`<identity>` and a newline on stdout; on stderr, when a cwd problem exists
and differs, a note that this directory still selects it (a broken cwd
problem is not an error for `switch`: the note says it could not be
resolved). Hooks (`on_switch`) run after a
successful switch once hooks exist (configuration contract §8.3); they are
not part of this transaction.

No command updates the saved selection implicitly (`add` and jobs do not).
A job resolves its problem once; a later switch does not retarget it.
Atomic replacement lets a concurrent reader see the old or the new
selection; it does not give a snapshot of other metadata.

Tests: the precedence matrix, including an invalid winner with a valid
lower source and an unreadable lower source with a valid winner; fresh
workspace → `NoSelection`; stale saved problem replaced by `switch`;
`malformed` and `too_new` state refused and left byte-identical; different
spelling, collision (`problems/A` and `problems/a`, Linux), link and
identity mismatch as references and as cwd problem; the current directory
entered through a link `problems/a` → `problems/b` selects `b`, through a
link to a directory outside `problems/` gives no cwd problem; `current`
running while `switch` replaces the state sees the old or the new
identity; an unknown-kind problem selectable; stdout purity of `current`;
`current` leaves the workspace byte-identical (no lock file created, no
recovery); same-selection `switch` writes nothing; `switch` inside another
problem prints the identity alone on stdout and the note on stderr; configuration loaded for `--problem` B while the
current directory is in A uses B's `[config] language`.

### 5.8 `cpf fetch`: fetch transaction and test publication *(fetch amendment, draft)*

Publishes a remote problem's metadata and sample tests. HTTP, site adapters
and canonical sample bytes belong to the sites contract; this section starts
from a parsed problem and ends with committed records. Test files are never
modified or deleted: an update publishes new files and changes the catalog.
There are no generation directories.

#### 5.8.1 Adapter result (the interface this section consumes)

- **Parsed problem**: `identity` (a remote identity, §2.1, from the adapter's
  URL resolution, never from page text); `url` (the adapter's canonical URL,
  ≤ 2048 bytes); metadata fields `title` (≤ 1024 bytes), `kind` (string
  ≤ 64 bytes), `limits.time_ms`, `limits.memory_bytes` (positive integers
  ≤ 2^53), each either **available** or **unavailable** in this response;
  `samples`: an ordered list of 0–64 pairs `(input, answer)` of canonical
  bytes, both always present (either may be empty); `fetched_at` (response
  time). The **ordinal** of a pair is its position, from 1.
- Zero samples are a result only for a page the adapter positively
  recognized; more than 64 pairs, or an identity other than the requested
  one, is an adapter failure. An adapter failure produces no parsed problem;
  the attempt then records only its status (§5.8.9).
- **Parsed contest**: `identity`, `url`, `title` (optional), ordered
  `members`, each a label (contest record label grammar, §5.2) and a remote
  problem identity, at most the bulk cap of the sites contract.

#### 5.8.2 Records

`<p>` is the problem directory name (`<site>-<native>`, §2.1).

| Record | Class | Written | Meaning |
|---|---|---|---|
| `problems/<p>/tests.toml`: entries with `download`, table `[fetch]` | shared; tool fields (§8.1) | step 5 (creation: step 4) | **commit record**: test membership; `[fetch] txn` names the last committed fetch |
| `problems/<p>/problem.toml` `[problem]` `url`, `title`, `kind`, `limits` (`id` fixed at creation) | shared; tool fields | step 6 (creation: step 4) | last successful metadata |
| `contests/<c>/contest.toml` | shared | step 8 | contest membership |
| `.cpforge/fetch/<p>/journal.toml` | tool-only, no-clobber, removed at step 7 | step 3 | intent of the open transaction |
| `.cpforge/fetch/<p>/txn-<txn>.toml` | tool-only, no-clobber, **never rewritten** | step 7 | durable history of one transaction |
| `.cpforge/fetch/<p>/status.toml` | tool-only, atomic replace | end of each attempt | latest attempt, kept apart from the metadata |
| `.cpforge/downloads/<txn>/` | tool, leased | steps 0–1 | download area |

- **Transaction id** `<txn>`: the job-id grammar (§6.2), generated at step 0;
  the exclusive `create_dir` of the download area makes it unique (an
  existing name → new random part). History lists in txn order.
- **Encodings**: `sha256` values are 64 lowercase hex digits of SHA-256 over
  the file bytes; times are RFC 3339 UTC with second precision and `Z`
  (`2026-11-23T10:15:00Z`); `ordinal` is an integer 1–64.
- `problem.toml` shape (tool fields; all other items are user fields and
  round-trip untouched, §3):

  ```toml
  [problem]
  id = "atcoder:abc400_a"
  url = "https://atcoder.jp/contests/abc400/tasks/abc400_a"
  title = "Example"
  kind = "batch"
  limits = { time_ms = 2000, memory_bytes = 1073741824 }
  ```

  `limits` are published judge limits and never become local limits
  (configuration contract §7.2). An unavailable field is never written,
  changed or removed by that fetch.

#### 5.8.3 Download area

- Created at step 0 under the workspace lock: `create_dir`
  `.cpforge/downloads/<txn>/` (parents by the guard), then `lease` created
  with `create_new` and locked non-blockingly with the §6.2 lease primitive,
  before the lock is released. A lease is created only there, so a lease
  found free is never taken again.
- The owner writes the canonical sample bytes as `<k>.in` and `<k>.ans`
  (`k` = ordinal), computing SHA-256 while writing. Nothing else writes in
  the area. Nothing in it is synced: a crash discards it.
- **Reclamation** is part of leftover recovery (§4.5): an entry named by a
  txn id is probed with an independently opened handle (§6.2 outcomes).
  `held` → kept; `acquired` or `absent` → probe released and closed, the
  directory renamed to `.reclaim-<txn>` and deleted with the no-follow
  walker (§2); `failed(e)` → kept and reported (`RecoveryIncomplete`). An
  entry `.reclaim-<txn>` is deleted (an entry vanishing meanwhile is
  success). Any other entry is kept and reported by `fsck`. A process's
  own probe of its own held area returns `held`.
- **Owner release** (end of step 7, and after any abort): under the lock,
  drop the lease handle, rename the area to `.reclaim-<txn>`, release the
  lock, delete it. A failed rename or deletion is left for recovery.

#### 5.8.4 Order of one problem transaction

**Bases** are recorded at step 0: whether `problems/<p>` exists; the SHA-256
of the bytes of `problem.toml` and of `tests.toml` (or `absent`); for every
**download entry** (an entry with a `download` table, §8.1), the state of its
`input` and `answer` file: SHA-256 of a regular file (links followed, §8.1
health), or `missing` (absent, not a regular file, or unreadable).

0. **Prepare** (lock, §4.5 recovery). Reconcile a journal of `<p>`
   (§5.8.7). Then refuse, with nothing created, when: the problem exists and
   `problem.toml` is not `valid` or its identity is not the requested one;
   it does not exist and the §2.1 resolution of `<p>` is not "create"; the
   catalog is not `missing` or `valid` (§8.1); a journal of `<p>` remains
   (classification other than `valid`, or reconciliation left it).
   Otherwise create the download area, record the bases, release the lock.
1. **Network phase**, without the workspace lock: the adapter produces the
   parsed problem, whose samples are written into the area.
2. **Revalidate** (lock, recovery). A journal of `<p>` present now is
   reconciled first. Then every base is recomputed; any difference is the
   conflict `base_changed`: nothing is published, the status is recorded.
   **Editedness is observed here** (it equals the step-0 observation). The
   plan is computed under this lock: pair outcomes (§5.8.5), file names
   (§5.8.6), catalog operations, metadata diff (one `(field, old, new)` per
   available field whose value differs). A plan with no file, no catalog
   operation, an unchanged `[fetch] upstream_samples` and no metadata diff
   is `unchanged`: no txn record, no journal, status only. A plan whose
   catalog, journal or history record would exceed 1 MiB (§9) is refused
   (`record_too_large`) before step 3.
3. **Journal**: publish `.cpforge/fetch/<p>/journal.toml` no-clobber (§4.3,
   synced): `schema`, `txn`, `identity`, `create` (bool), the bases, the
   planned files (path, sha256, side, ordinal, role), the catalog operations
   and the metadata diff. A refetch publishes from the area directly; only
   creation stages.
4. **Publish files** no-clobber (§4.3), in plan order (input before answer
   of each pair), each streamed from the area. The SHA-256 of the bytes
   written to the temporary file must equal the planned hash before the
   publishing call; a mismatch or `Exists` stops publication and aborts
   (§5.8.8, `failed` or `name_taken`). Each publication syncs the file
   before its rename and the directory after it (§4.3).
5. **Commit the catalog**: one §4.4 edit of `tests.toml` without the replay
   wrapper, applying the journal's operations and setting `[fetch] txn` and
   `upstream_samples`. A missing catalog is first created no-clobber as
   exactly `schema = 1\n` (as §5.3). **Each publish attempt** (§4.4 step 4,
   every Windows retry included) also rehashes (a) the input and answer of
   every entry the edit replaces, against their bases, and (b) every file
   this txn published that the edit registers, against its planned hash. A
   difference is `Conflict`: nothing is committed and the transaction
   aborts (§5.8.8, `conflict`, reason `edited_during_commit`).
6. **Metadata**: one §4.4 edit of `problem.toml` with the replay wrapper.
   For each diff field: current = old → set new; current = new → nothing;
   otherwise kept and reported (`metadata_conflict{field}`). A second
   `Conflict`, or any other failure, leaves the journal: the tests are
   committed with old metadata (allowed, reported, §5.8.12) until
   reconciliation rolls step 6 forward. The status is then `committed` with
   the finding `metadata_pending`, and the area is released. A
   `problem.toml` that is no longer `valid`, or no longer bound to the
   identity, is not edited: finding `metadata_not_applied`, and step 7
   follows (the journal never waits on a user repair).
7. **Finish**: publish `txn-<txn>.toml` no-clobber; replace `status.toml`;
   remove the journal (unlink, then sync `.cpforge/fetch/<p>/`); release
   the area (§5.8.3).

**Creation** (no `problems/<p>` at steps 0 and 2): step 3 writes the journal
with `create = true`; step 4 stages `.cpforge/staging/<txn>/` with the same
problem files as §5.2 step 2 (identity, template solution under the same
rules, `[files] created`), the tool fields of `problem.toml`, `tests.toml`
(`schema = 1`, `[fetch]`, one entry per pair) and the sample files under
the base names, then `publish_dir_noclobber` to `problems/<p>`. That
publication commits steps 4–6 at once; a non-committed publication is
reclassified by §2.1 and aborts with nothing published. Step 7 follows.
Zero samples give an empty catalog (`upstream_samples = 0`, no entries) and
the warning `no_samples`.

**Problem sets of several problems** run one transaction per problem, in
selection order, each with its own txn, area and lock acquisitions; a
contest selection then runs step 8.

#### 5.8.5 Pair rules

- A **sample** is an (input, answer) pair. A download entry is **edited**
  when either file's state at the bases differs from its `download` hash
  (bytes differ, or `missing`). Its **recorded pair** is
  `(input_sha256, answer_sha256)`; for an upstream pair `U_k`, the hashes of
  its canonical bytes.
- `n` = number of upstream pairs; `n_prev` = `[fetch] upstream_samples`
  (0 when `[fetch]` is absent).
- **Identical upstream**: every download entry's ordinal `k` has
  `U_k` = its recorded pair, and no `U_k` lacks an entry → no pair
  outcome; the checks below are skipped.
- **Zero samples**: `n = 0` with at least one download entry → the test set
  is kept: no pair outcome, no catalog operation, `upstream_samples`
  unchanged, `--disable-removed` ignored, warning `zero_samples`.
- **Ambiguity** otherwise: any of the following is a *conflict* of the whole
  problem: nothing is published, registered or changed (metadata
  included), the status records each finding.
  - `duplicate_ordinal`: two download entries with one ordinal;
  - `duplicate_recorded`: two download entries with equal recorded pairs;
  - `duplicate_upstream`: two equal upstream pairs;
  - `moved`: an upstream pair `U_k` that differs from the recorded pair of
    entry `k` (or has no entry `k`) and whose input hash equals the recorded
    input hash of an entry with ordinal `j ≠ k` (renumbering, permutation).
- **Matching**: upstream pair `k` matches the download entry with ordinal
  `k`, which the checks above make unique.
- **Outcome table** (per ordinal `k`, after the checks; "disabled" changes
  nothing in a row except where stated):

| Local state of `k` | Upstream `U_k` | Outcome |
|---|---|---|
| no entry, `k > n_prev` | present | `added`: publish under the base name if free, else a revision name (§5.8.6); append entry `id` = stem, `origin = "sample"`, `enabled = true`, `download`; files role `registered` |
| no entry, `k ≤ n_prev` | present | `removed_locally`: nothing published or added |
| no entry | absent | nothing |
| entry, unedited | = recorded | `unchanged` |
| entry, unedited | ≠ recorded | `updated`: publish a revision pair; replace the entry's `input`, `answer`, `download` (id, `enabled` and unknown keys kept, so a disabled entry stays disabled); new files `registered`, old files `superseded` |
| entry, edited (input, answer or both; a file missing) | = recorded | `kept_edited`: nothing |
| entry, edited | ≠ recorded | `kept_edited`: the whole pair and its entry are kept; the upstream pair is published as an unregistered revision pair (role `unregistered-upstream`) unless already retained (below) |
| entry (any) | absent (`k > n ≥ 1`) | `removed_upstream`: reported; with `--disable-removed` the edit sets `enabled = false`; files untouched |

- **Already retained**: a valid history record of `<p>` (§5.8.10) lists,
  for ordinal `k`, an `unregistered-upstream` pair with exactly `U_k`'s
  hashes, both files are present with those hashes, and no catalog entry
  names either → nothing is published; the outcome names that pair.
- Catalog entries without `download` (custom, imported, stress, or a
  `sample` entry written by hand) take no part in matching; their paths
  still reserve names (§5.8.6).
- `cpf test register` may register any unregistered candidate, including a
  superseded file or an unregistered upstream pair; the entry is
  `origin = "custom"` without `download` (§5.3) and takes precedence over
  the history (§5.8.10).

#### 5.8.6 File names

- A pair is published under one stem `s`: `tests/<s>.in`, `tests/<s>.ans`.
  The base stem of ordinal `k` is `sample-<k>`; a revision stem is
  `sample-<k>.r<m>`, `m ≥ 2` in decimal without leading zeros.
- A stem is **free** when, by collision key (§2.1), neither file name equals
  an entry of `tests/` (any type), a path named by a catalog entry (even a
  missing file), a name planned earlier in this plan, or a path listed in a
  valid history record of `<p>`; and, for an `added` entry, `s` is no
  catalog id. Names are chosen under the step-2 lock.
- `added` uses the base stem if free, else the smallest free revision stem;
  `updated` and `unregistered-upstream` always use the smallest free
  revision stem. An existing unregistered `sample-1.r2.in` (or any file of
  that name) makes the choice skip to `r3`. A name is never reused for other
  content, so a name and hash pair identifies one publication.

#### 5.8.7 Reconciliation of a journal

Runs under the lock: in `cpf fetch` at steps 0 and 2 for that problem, and in
`cpf fsck --apply` for every journal. `cpf fsck` and readers only report.
Other commands do not reconcile.

- Classification (§9 record): `valid` → below. `too_new`, `unreadable`,
  `malformed` → kept, reported, and `cpf fetch` of `<p>` refused. `fsck
  --apply` removes a `malformed` journal (reported); the files it named are
  then user content. `too_new` and `unreadable` journals are never removed.
- **Committed?** Refetch: `tests.toml` is `valid` and `[fetch] txn` equals the
  journal's txn. Creation: `problems/<p>` is a directory bound (§2.1) to the
  journal's identity whose `tests.toml` `[fetch] txn` equals the txn.
- In every branch below, a `txn-<txn>.toml` already present and `valid`
  for that txn is kept, never rewritten, and a not-committed journal with
  such a record is always rolled back (the transaction was decided).
- **Committed** → roll step 6 forward (per-field rule and
  `metadata_not_applied`, §5.8.4) and step 7; status `committed` with
  `recovered = true`.
- **Not committed, refetch.** *Present set* = planned files present as
  regular files with their planned hash. Roll forward (steps 5–7, outcome
  `committed`, `recovered = true`) only if the present set is every planned
  file, every base still matches (a base `absent` catalog also matches a
  file of exactly `schema = 1\n`) and the step-5 rehash holds. Otherwise
  **roll back**: a history record with outcome `rolled_back` listing the
  present set as `unreferenced` (written only when the present set is not
  empty), status `rolled_back`, journal removed. No file is ever deleted.
- **Not committed, creation**: never rolled forward (its staging is removed
  by recovery); status `rolled_back`, journal removed.
- A planned name present with another hash, or as another file type, is
  user content: not recorded, never deleted (§5.8.10).

#### 5.8.8 Abort of a live transaction

An abort after step 3 (publication failure, `name_taken`, step-5 conflict) is
the roll back of §5.8.7 performed by the owner under its lock: history with
outcome `conflict` or `failed` listing the files this txn published as
`unreferenced`, status, journal removal, area release. The published files
stay as unregistered downloads. Before step 3 an abort writes only the status.

#### 5.8.9 Status record

`status.toml`: `schema = 1`, `attempted_at`, `outcome` (`committed`,
`unchanged`, `conflict`, `refused`, `failed`, `rolled_back`), optional
`txn` (when a journal was written), `recovered` (bool), `reason` (≤ 1024
bytes), and `[[finding]]` records (`kind`, optional `ordinal`, optional
`test`), at most 128. Written by atomic replace under the lock at the end of
every attempt that reached step 0, including adapter failures (a short lock
acquisition after step 1). A status that cannot be written (lock `Busy`,
I/O error, a `too_new` status, which is never replaced) is a warning of the
command; nothing else depends on it.

#### 5.8.10 History and provenance

- `txn-<txn>.toml`: `schema = 1`, `txn`, `identity`, `outcome` (`committed`,
  `rolled_back`, `conflict`, `failed`), `fetched_at`, `url`, `[[pair]]`
  (`ordinal`, `outcome` of §5.8.5) and `[[file]]` (`path`, `sha256`, `side`
  `input` | `answer`, `ordinal`, `role`, optional `test`). It lists every
  file the txn published (`registered`, `unregistered-upstream`,
  `unreferenced`) and every file whose entry it replaced (`superseded`,
  with the hash of the replaced `download`). Published no-clobber, never
  rewritten or removed by cpf. Bounded by construction (64 pairs, four
  files per pair, cpf-chosen names, catalog paths ≤ 255 bytes) and checked
  at step 2.
- **Tool-published**: a file under `tests/` counts as published by cpf only
  when its name and current SHA-256 appear in the open journal or in a
  `valid` history record of `<p>` whose `identity` is the problem's
  identity. Anything else is user content. Records of another identity
  (a directory name reused) or not `valid` contribute nothing.
- **Provenance** (workspace design §2.2, amended): references in
  `tests.toml` / `problem.toml` first (`registered`); then
  `downloaded{role, txn}` from the rule above; then `[files] created`,
  `[files] user`, otherwise `unknown`. A registered download entry whose
  file differs from its `download` hash is reported as edited.
- Displays: `cpf status` shows counts by role and the latest txn; `cpf fetch
  --history` lists every record (a non-`valid` one by classification).
  Records move with their problem into an archive.

#### 5.8.11 Observation points and the remaining race

- Editedness and every base are observed at step 0 and revalidated at
  step 2; samples are rehashed again in every step-5 publish attempt; the
  catalog itself is validated by §4.4 step 4.
- **Remaining race.** §4.4 is not compare-and-swap with editors. An editor
  save to a replaced sample file after the last step-5 rehash and before the
  catalog rename is not detected: no file is modified or deleted, but the
  catalog then names the upstream revision. `cpf status` reports the
  superseded pair as edited (its hash differs from its history record).
  A save to `problem.toml` after step 2 is handled field by field (step 6).
  Stronger semantics would need generation directories.

#### 5.8.12 Readers

Lock-free readers use the committed `tests.toml` and `problem.toml`. While a
journal of `<p>` exists (any classification) `cpf status` and `cpf test ls`
report `fetch_incomplete`, with `pending` (not committed: the planned files
are unregistered candidates) or `committed` (tests committed, metadata or
history pending: old metadata with new tests). Jobs read the committed
catalog; nothing they select depends on a journal.

#### 5.8.13 Contest membership (step 8)

- A contest selection resolves the parsed contest first, then runs one
  transaction per member, then one membership edit under the lock.
- `contests/<site>-<native>/contest.toml` carries `[contest] id =
  "<site>:<native>"` (bound to the directory like a problem, §2.1),
  `url`, `title` (tool fields). Absent → created no-clobber with `schema =
  1` and `[contest]` before the edit (as §5.2); present with another or no
  `[contest] id` → refused (`IdentityMismatch`).
- The edit adds a record for each member that is **committed now**:
  `problems/<dir>` is a directory bound to that identity with a `valid`
  `problem.toml`. A member whose transaction failed but which is committed
  from an earlier fetch is included; an uncommitted member never is.
- Order: a new record is inserted before the first existing record whose
  member comes later in the enumeration; records not in the enumeration keep
  their position. An existing record keeps its stored label (a different
  upstream label is reported); a label held by another record is omitted
  from the new record and reported.
- Fetch never writes or changes per-contest overrides (`limits` and other
  record keys are user keys, §5.2); an upstream limit change edits only
  `problem.toml`.
- Problem-set sections (not contests) write no membership.

## 6. Job directories, leases, pruning

### 6.1 Layout

```
.cpforge/runs/<job-id>/            time-ordered + random id, never reused
    lease                          lock file held by the job owner
    job.toml                       (draft) schema, (epoch, n) admission sequence
    report.json
    inputs/<test-id>.in|.ans       frozen copies (§7)
    inputs/src/...                 (draft) captured solution-target sources (§7)
    inputs/capture.json            capture manifest (report contract §4.2)
    builds/<build-id>/             fresh output directory per build (build planning)
    <run-id>/  spec.json record.json cleanup.log  out/stdout out/stderr  cwd/
.cpforge/runs/.reclaim-<job-id>/   being deleted
```

`<run-id>` is `r` + the five-digit admission index (report contract §3.4);
`<build-id>` is the build's run id.

Builds live inside their job (every test rebuilds, design §6.6), so nothing
is shared between jobs.

### 6.2 Admission and lifetime

The workspace lock is held **continuously** from step 1 to step 5:

1. Lock; leftover recovery (§4.5).
2. Prune steps 1–2 (§6.3): select and rename candidates to `.reclaim-*`.
2a. *(draft)* Allocate the admission sequence: classify `job-seq` (§9);
   `missing` or `malformed` → admission fails closed ("run `cpf fsck
   --apply`"); `unreadable` → fails closed naming the I/O error ("fix access
   to `.cpforge/job-seq`, or remove it and run `cpf fsck --apply`");
   `too_new` → fails closed ("needs a newer cpf"); `next = u64::MAX` →
   refused (`SequenceExhausted`). Otherwise atomically replace it with `next + 1` (§4.2); the
   job's sequence is `(epoch, next)`.
3. Create `runs/<job-id>/` (`create_dir`, fresh id).
3a. *(draft)* Publish `job.toml` no-clobber with the sequence.
4. Create `lease` and lock it non-blockingly (new file: must succeed).
5. Release the workspace lock.
6. Prune step 3: delete the renamed `.reclaim-*` directories, outside the lock.

The **job owner** is one shared handle holding the lease; runs, the cleanup
service's transferred runs and persistence threads (execution contract §§3.3,
6.2) and the report writer hold clones. The
lease is released when the last clone drops.

**Inactive** = no cooperating owner holds the lease (the job finished or its
`cpf` died). A job directory **without a `lease` file** is an interrupted
admission and inactive; a lease that exists but cannot be opened
(permission, I/O error) leaves the directory in place and is reported.

**Registered and ready** (decisions of 2026-11-20). Steps 1–5 produce a
**registered** job: sequence allocated, directory, `job.toml` and held
lease. Nothing can run from a registered job. Freezing (§7: selected tests,
then the captured files) runs outside the workspace lock under the held
lease, and only its complete success, together with planning of the
source-consuming stage (§7 capture check and accident guard), produces a
**ready** job, the only value from which runs can be started. A failure
after registration leaves an inactive directory (possibly with partial
copies) for pruning, never a usable manifest; its sequence number stays a
gap. Admission that fails after step 2 may already have renamed prune
candidates: fail closed means no new job runs, not no filesystem change; the
renamed `.reclaim-*` directories are still deleted (step 6) or retried by a
later prune.

- **Job id**: `<UTC time, 17 digits yyyymmddHHMMSSmmm>-<16 lowercase hex
  random>`, created with `create_dir` (an existing name → new random part).
  "Oldest" (§6.3) orders by the id's byte order (time, then random part as
  a tie-breaker); directory timestamps are never used. A clock set back
  can misorder jobs; that only changes which inactive job is pruned first.
- **`job.toml`**: `schema = 1`, `epoch`, `n` (§9 encodings), nothing else in
  the CLI foundation. The capture check and the captured file lists are
  persisted in the capture manifest `inputs/capture.json` (report contract
  §4.2), published no-clobber as the last capture step with its size
  reserved first; a manifest that is not committed is a capture failure and
  the job never becomes ready. They are not persisted in `job.toml`.
  Reports carry bounded previews, the list digest and the manifest path;
  facts carry the capture check (§6.5).
- **Lease**: a regular file `lease` inside the job directory, locked with the
  §4.5 backend (`flock(LOCK_EX | LOCK_NB)` on Unix; `LockFileEx` exclusive,
  fail-immediately, on a one-byte range on Windows). Owner and probe
  handles are opened independently and are not inherited by children; on
  Windows both open with read, write and delete sharing (so neither blocks
  the other); the job directory itself cannot be renamed while either is
  open (§6.3 step 2 releases the probe first). Probe outcomes are kept
  distinct: `held` (skip), `acquired` (inactive), `absent` (interrupted
  admission, inactive), `failed(e)` (cannot open or lock for another
  reason: never treated as inactive; the directory stays and is reported).
- **Stale recovery** means reclaiming inactive storage only. Nothing
  decides from age or process ids that a job died, and nothing rebuilds its
  report or publishes its fact; repairing `job-seq` is `fsck`'s.
- **Fingerprints** (§6.5) are computed later from the private copies (they
  are tool-owned and never modified after capture), so capture records the
  ordered source and support lists and the copy paths, and run-fact publication (§6.5) hashes the
  copies as written.

**Limitation:** after an abrupt `cpf` death, Unix descendants outside
containment (execution contract `setsid` limitation) can keep writing. Pruning removes the
names, but storage held by open files is released only when those writers
exit, and such writes are invisible to measurement. On Windows, kill-on-close
ends job members with the `cpf` process (execution contract §4.3).

### 6.3 Pruning

Runs inside job admission (§6.2, which keeps the lock across steps 1–2 and
its own directory creation) and in `cpf clean` (steps 1–3 directly). *(draft)*
`cpf clean` plans by default and with `--apply` prunes with a **zero**
retention budget (`keep_jobs = 0`, `workspace_bytes = 0`): every inactive
job directory is a candidate except newer-schema ones (§9, reported);
active ones are skipped by the lease probe. In the initial feature release it also deletes
`.cpforge/cache/`. The plan mode takes the lock and so performs the
universal §4.5 leftover recovery, but selects and renames nothing.
It never touches anything else.

1. Under the workspace lock, measure (§6.4) and list inactive job
   directories oldest first (by job id, §6.2). Active jobs count towards the
   measured usage and are never candidates; `keep_jobs` counts inactive
   jobs. A job directory holding a **newer-schema** `job.toml`, `spec.json`,
   `record.json` or `report.json` is never a candidate (reported); one whose
   `job.toml` is missing or malformed (interrupted admission) is a normal
   candidate; a record that cannot be read leaves the directory in place
   (reported), since its schema is unknown. Candidates: beyond the newest `keep_jobs`
   (*default* 20) inactive jobs; then, using **projected** usage (measured
   usage minus the measured size of candidates already selected), further
   oldest inactive jobs while projected usage exceeds `workspace_bytes`
   (*default* 4 GiB). Measurement is `known bytes + incomplete: bool`; an
   incomplete measurement selects by `keep_jobs` only and is reported.
2. For each candidate: try-lock its lease with a probe handle (held → skip;
   absent → interrupted admission, proceed); then **release the probe lock
   and close its handle**, and rename the directory to
   `runs/.reclaim-<job-id>`. Windows cannot rename a directory while any
   file inside it is open, whatever its sharing mode (CI, 2026-11-20), so no
   handle may stay open across the rename. Releasing first is safe: pruning
   is serialised by the workspace lock, and a lease found free is never
   taken again by an owner (leases are created only at admission, with
   `create_new`); another lock-free probe holding it briefly only makes a
   later prune skip the job. A failed rename (on Windows also caused by a
   handle another process keeps open inside the directory) leaves the
   directory in place (reported, retried next time).
3. Release the workspace lock; delete each `.reclaim-*` with
   the no-follow walker (§2). Leftovers stay as `.reclaim-*` and are retried by the
   next prune. Two processes may delete the same `.reclaim-*` tree at once:
   an entry that disappears during deletion is success, never an error. A
   rename never overwrites an existing `.reclaim-<job-id>`: that candidate is
   skipped and reported. Projected usage subtracts a candidate only after its
   rename succeeded.

### 6.4 Retention

- **Owners** (decisions of 2026-11-20): `keep_jobs` and `workspace_bytes` are enforced
  by pruning (§6.3); `job_bytes` by the shared reservation interface below,
  which every producer of job files uses (admission and freezing write
  `job.toml` and the copies; later the run, record and report writers);
  `cwd_bytes` by run finalization; the build cap by build finalization
  (build planning).
- **Per job** (`job_bytes`, *default* 1 GiB): exact, in-process, for bytes
  `cpf` itself writes for the job (captures, frozen inputs, specs, records,
  reports, logs), through the execution contract's reservation interface (execution contract §5.4).
- **Workspace** (`workspace_bytes`): a **soft pruning target**, not a
  ceiling. No cross-process reservations; admission is never refused for
  workspace usage. Usage is measured by no-follow traversal (§2) of `runs/`
  (logical lengths; hard links and reflinks at full length); entries that
  cannot be measured are reported, not counted as zero and not blocking.
- Usage can exceed the target, and **there is no convergence guarantee**:
  running jobs, compiler and child output, escaped descendants, files held
  open after deletion (§6.2), failed deletions, protected newer-schema
  directories and incomplete measurements can all keep it above the target
  until a later prune succeeds.
- **Per-job accounting definition:** outstanding logical bytes of files `cpf`
  creates for the job, admitted through the execution contract §5.4 reservation **before**
  each allocation (captures, frozen input copies and clones, file-I/O copies
  into `cwd/`, specs, records, reports, logs, the capture manifest and the
  report reservation (report contract §3.7), and their temp files during
  replacement), with checked arithmetic; released when the file is removed or
  when the job's owner is released. Reflinks count at full logical length.
- **Child output:** after each run, `cwd/` beyond `cwd_bytes` (*default*
  256 MiB) is cleared and `CwdOverflow{bytes}` recorded (diagnostic, not a
  verdict). After each build, output beyond the build cap (configuration contract) →
  `BuildOutputTooLarge`, the build directory is removed (a failed removal is
  reported) and nothing runs from it.

### 6.5 Run facts *(draft)*

- One fact per (problem, solution target): `.cpforge/facts/<problem>/
  <target>.toml` (`schema`, `epoch`, `n`, job id, finish time, verdict
  summary, `source` and `tests` fingerprints, workspace design §3.2 encoding,
  and the job's capture check, §7).
- An existing fact and `job-seq` are classified by §9's **record
  classification** (`missing`, `unreadable`, `malformed`, `too_new`,
  `valid`). Fact fields validated for `valid`: `epoch` (§9 encoding), `n`
  (§9 encoding), job id, finish time, verdict summary, both fingerprints
  (64 lowercase hex characters) and the capture check.
- **Publication** in report phase A (report contract §2.9) by a job whose
  primary verdict is not `cancelled`, `infrastructure` or
  `internal_error`, **before** the lease is released: lock; read
  `job-seq`, then the existing fact. After reading `job-seq` and the
  existing fact, the job reads its cancellation intent once (the
  **cancellation cutoff**); intent set → `NotAttempted{Cancelled}`, nothing
  written. Intent set after the cutoff does not prevent the replace.
  Otherwise decide by the first matching row; release the lock.

  | Condition | Outcome |
  |---|---|
  | `job-seq` not `valid` (`missing`, `unreadable`, `malformed`, `too_new`) | `NotPublished{SequenceUnavailable}` |
  | `job-seq` epoch ≠ job epoch | `NotPublished{EpochChanged}` |
  | existing fact `too_new` | `NotPublished{FactTooNew}` (§9: no mutation) |
  | existing fact `unreadable` | `NotPublished{FactUnreadable(e)}`, file left as it is |
  | existing fact classified `valid`, of the same epoch, with `n` ≥ job `n` | `Superseded` (no change) |
  | otherwise (fact `missing` or `malformed`; `valid` of another epoch or with lower `n`) | atomic replace (§4.2) → `Published`, or `NotPublished{Io(e)}` if not committed |

- The replace is the **commit point**. Post-commit `Durability::Uncertain`
  or `Cleanup::Pending` (§4.1) are warnings on a `Published` outcome.
- **Lock failure**: the workspace lock not acquired (`Busy`, §4.5) →
  `NotPublished{LockBusy}`. Replacement eligibility follows the decision
  table above; `unreadable` and `too_new` facts are always preserved.
- Every outcome is recorded in the job report; `NotPublished` and warnings
  are **warnings** of the job, never a verdict change; they do not change
  the job's exit code (report contract §5.2). A job cancelled before the
  cutoff, or interrupted (death) before the commit, leaves the previous fact
  unchanged; cancellation after the cutoff does not stop the replace, and a
  committed fact is never rolled back; `status` calls it the last
  completed run. Nothing is recovered from interrupted jobs.

## 7. Frozen inputs and execution directories

- **Freeze at job start:** the job resolves its test list once, copies every
  input and answer it will use into `inputs/` (reflink where supported, else
  byte copy; regular files only), hashes each copy and completes a **frozen
  manifest** (test ids, answer absent / empty / present, hashes, resolved
  per-test settings) before the first run. A freeze failure starts no run.
- The result is an **immutable collection of captured bytes**, with no claim
  that the files existed in that combination at one instant: a file edited
  while being copied can be captured mixed, and an input and its answer can
  come from different moments. Every run reads only these copies; edits after
  the freeze affect the next job only.
- Each run gets a new empty `cwd/`; build artefacts are referenced by
  absolute path; for file-I/O problems the frozen input is copied into `cwd/`
  under its validated name (counted in the job's budget) and the declared
  output is read from there.
- `TestStore::freeze_into` writes only into an admitted job's `inputs/`
  through the `JobInputs` capability from admission.
- *(draft)* **Source capture**. A job's **admission is complete
  only after its freeze**: the §6.2 workspace-lock section (steps 1–5) ends
  with the lease held and the lock released; the freeze then runs outside
  the lock: tests (above), then the selected solution target's **captured
  files** (its `sources` followed by its `support_files`, configuration
  contract §6.2; build planning target resolution) copied into `inputs/src/` with their
  problem-relative paths (same copy rules, regular files only, counted in
  the job budget). Each destination file is created **exclusively**
  (`create_new`); an existing destination (a filesystem alias the plan's
  same-file check missed, e.g. after a rename between plan and capture) →
  capture error `SourceCollision{path}`, nothing overwritten. No build or
  run starts before the freeze is complete; a capture failure starts
  none.
- **Fingerprint**: the `source` fingerprint is computed with the
  exact byte encoding of workspace design §3.2, streaming the **copied bytes as written**
  into it (not per-file digests); `status` uses the same encoding over the
  current files.
- **Sources come from the copy**: in **every** stage of the job (build,
  run, command with `needs_build`), `{sources}` and `{source}`
  (configuration contract §5.2) expand to the `inputs/src/` paths, in the
  configured order. The **source-consuming stage** is the build stage, or
  the run stage of a build-free target (e.g. Python). Helper files a
  solution needs (headers, modules) are declared as sources of its target,
  so they are captured.
- **Accident guard** (phase-3 error, configuration contract §3), applied to
  the source-consuming stage only:
  1. It must not use `{problem_dir}` or `{workspace}`, a `cwd` selector
     `problem` or `workspace`, or a `cwd` path that resolves into the
     workspace outside the job directory.
  2. **Operands.** Each expanded `argv` element, and each expanded shell
     `args` element, yields candidate operands: the element itself; if it
     contains `=`, the text after the first `=`; if it starts with a single
     `-` (on Windows also `/`) and is longer than two characters, the text
     after its first two characters (`-I…`, `-L…`, `-o…`, MSVC `/I…`). A candidate is a **path operand**
     when it is absolute or contains a path separator (`/`; also `\` on
     Windows). Each path operand is resolved **lexically** against the
     stage's `cwd` (absolute operands as they are; `.` removed, `..`
     applied; no link following) and compared **component-wise** with the
     workspace root as `cpf` located it (ASCII case-insensitive on Windows
     and macOS). A path operand inside the workspace but outside the job's
     own directory is rejected. Paths inside the job directory (its
     `inputs/src/`, build output root, `cwd`) and paths outside the
     workspace are accepted. **Every** candidate of an element is checked
     and one rejected candidate rejects the element. This is deliberately
     conservative: the whole-element candidate of `-I../../../../../ext`
     or `-Wl,-rpath,../../../../lib` can resolve into the workspace even
     when the intended path does not; such an argument is rejected and is
     rewritten (an absolute path outside the workspace) or moved into a
     shell stage.
  3. Elements are not split further: comma or `PATH`-style lists inside
     one element yield no extra candidates (the whole element is still a
     candidate under rule 2). Paths reached through another spelling of the
     workspace (a link or alias to it) and the text of a shell **script**
     are not inspected.
- **Capture check** (decided by the plan; recorded in the job report and the fact,
  §6.5): `checked` when the source-consuming stage is an `argv` stage that
  passed the guard; `unchecked{shell_stage}` when it is a shell stage
  (its args are guarded, its script is not inspected). `checked` means only
  that the guard passed: the build is not sandboxed, and reads through
  includes inside sources, environment variables or wrapper programs are
  not detected (workspace design §3.2). `cpf config` and `cpf doctor` show
  the capture check each target's stages would get.
- Literal paths in a command that point outside the workspace (compilers,
  system headers, libraries) are external inputs, like the toolchain, and
  are not covered by any fingerprint.

## 8. Test-store interface

```rust
pub trait TestStore {
    fn list(&self) -> Result<Vec<TestEntry>, StorageError>;   // tests.toml + discovery
    fn add(&mut self, g: &WorkspaceGuard, input: &CapturedFile, answer: Option<&CapturedFile>, origin: Origin) -> Result<AddOutcome, StorageError>;
    // CapturedFile: a pre-lock capture (§5.3) read as a stream, never as one buffer
    fn set_enabled(&mut self, g: &WorkspaceGuard, id: &TestId, enabled: bool) -> Result<(), StorageError>;
    fn freeze_into(&self, ids: &[TestId], job: &JobInputs) -> Result<FrozenManifest, StorageError>;
}
```

`TestEntry` distinguishes answer absent / present-empty / present and
registered / unregistered; `AddOutcome` reports partial results (§5.3).
The trait above is the target shape; `freeze_into` arrives with job
admission (it needs `JobInputs`), and the CLI foundation may grow the store
in that order.

### 8.1 Test catalog (`tests.toml`)

User-editable (§3, §9; classification, 1 MiB cap, unknown keys and comments
preserved by edits):

```toml
schema = 1

[[test]]
id = "custom-1"
input = "tests/custom-1.in"
answer = "tests/custom-1.ans"   # omitted: the test has no reference answer
origin = "custom"               # "sample" | "custom" | "stress" | "imported"
enabled = true
```

- `id`, `input`, `origin`, `enabled` are required; `answer` is optional.
  `id` is an Identifier (§2.1), unique within the problem by collision key;
  it is stable: editing an entry's paths keeps it, and renaming a file on
  disk does not change the catalog (the entry then reports a missing file).
- `input` and `answer` are `tests/<name>` with `<name>` a portable component
  (§2.1), `/` as separator; anything else (absolute, `..`, nested
  directories, `\`) is `malformed`. No two entries, and not an entry's input
  and answer, may name paths with equal collision keys. Order of `[[test]]`
  is the catalog order.
- **Answers:** `answer` omitted means deliberately absent; an `answer`
  naming a zero-byte file is present and empty; an `answer` naming a missing
  file is an entry error. A registered entry's answer is never inferred from
  a sibling file. Whether a test without an answer can pass is decided by
  the comparator and job tasks.
- No hashes for `custom` tests: the current file is the content, and job
  admission hashes the bytes it copies (§7). Download hashes arrive with
  fetch and mean "content at download time".
- *(fetch, draft)* **Download fields** (tool fields, written only by fetch,
  §5.8):

  ```toml
  [fetch]
  txn = "20261123101500123-0123456789abcdef"   # last committed fetch
  upstream_samples = 3                         # upstream pair count at that fetch

  [[test]]
  id = "sample-1"
  input = "tests/sample-1.in"
  answer = "tests/sample-1.ans"
  origin = "sample"
  enabled = true
  download = { input_sha256 = "…", answer_sha256 = "…", ordinal = 1, fetched_at = "2026-11-23T10:15:00Z", txn = "…" }
  ```

  `download` is valid only on an entry with `origin = "sample"` and an
  `answer`; its five keys are required (encodings §5.8.2; `ordinal` 1–64,
  `txn` the job-id grammar); unknown keys inside it are preserved. `[fetch]`
  requires `txn` and `upstream_samples` (0–64). Any violation is
  `malformed` (§9), like every invalid known field. Two entries with one
  ordinal or equal hashes are **not** a catalog error: they make a refetch
  a conflict (§5.8.5). The hashes describe the content at download time; an
  entry whose files differ is *edited* and is still run as it is now.
  Fetched answers are always present; a zero-byte fetched answer is
  present-empty.
- Per-test checker overrides are a later design; until then such keys are
  preserved as unknown keys, and a recognised execution-affecting key that
  the running version does not support is a planning error, never ignored.
- **Classification** (§9 user-editable): `missing` (no file: an empty
  catalog; `add` and `register` create it), `unreadable`, `malformed`
  (including any invalid known field or duplicate), `too_new` (raw display
  only), `valid`. Only `missing` and `valid` permit mutation; the others
  refuse every command but `ls`, which reports the classification and still
  lists discovery candidates. *(fetch, draft)* `cpf fetch` refuses at step
  0 and again at step 2 (§5.8.4), before any request or publication.
- **Entry health** (checked at use, reported by `ls`, never a catalog
  error): the files must exist and resolve to regular files; test data is
  read-only, so links are followed (§2), FIFOs, sockets, devices and
  directories are refused. A file is opened **non-blocking** (Unix
  `O_NONBLOCK | O_CLOEXEC`; Windows opens never block on these types) and
  the type is validated on the **opened handle** before any read, so a
  regular file replaced by a FIFO between inspection and use is refused,
  never waited on. An unselected disabled entry's health is reported
  but never blocks a job; any selected entry that is unhealthy, including a
  disabled entry explicitly selected with `--case`, stops the job before any run.
- **Discovery** (read-only, never writes the catalog): entries of `tests/`
  (no recursion) named `<stem>.in` that no catalog entry names in either
  role are **unregistered candidates**, shown with their sibling
  `<stem>.ans` if present. A `.ans` no entry names and without a candidate
  `.in` is reported as a stray file. Candidates are never run, including
  through an explicit selector; `cpf test register` turns one into an entry.
  `.cpf-tmp-*` names are never candidates. *(fetch, draft)* Superseded
  downloads and unregistered upstream pairs (§5.8.5) are ordinary
  candidates: listed, never run, registrable by `cpf test register`.
The CLI foundation implements it over `tests.toml` + `tests/`. *(fetch,
draft)* Fetch keeps that model: no generations; the catalog edit of §5.8 is
the commit. Lock-free readers of metadata tolerate absence,
replacement between reads and malformed hand edits (reported with location);
captures, logs and build outputs are running artefacts until their job
finalizes them (execution contract §6.2).

## 9. Schema versions

- Governed formats: `cpforge.toml`, `problem.toml`, `tests.toml`,
  `contest.toml` (user-editable, ordered `[[problem]]` membership, §5.2);
  `state.toml`, `spec.json`, `record.json`, `report.json`, *(draft)* `job-seq`, `job.toml`, `facts/*.toml` (tool-only).
  *(fetch, draft)* Also `.cpforge/fetch/<p>/journal.toml`, `status.toml`
  and `txn-<txn>.toml` (tool-only, schema 1, unknown keys `malformed`, each
  field set of §5.8 owned by its writer). The `tests.toml` download fields
  (§8.1), the `problem.toml` tool fields and `contest.toml` `[contest]`
  (§5.8.13) are additions to schema 1 of those user-editable formats: a
  version without fetch preserves them as unknown keys.
- *(fetch, draft)* **Fetch records**: classified with the tool-only record
  classification below. Newer-schema handling: a `too_new` journal blocks
  `cpf fetch` of its problem and is never removed; a `too_new` history
  record is kept, never rewritten and contributes no provenance; a
  `too_new` status is not replaced. Size: every fetch record, and the
  catalog after a fetch edit, is checked against the cap before it is
  written (§5.8.4 step 2): exceeding it is the refusal `record_too_large`,
  never a truncation. With at most 64 pairs (§5.8.1) and cpf-chosen names,
  a fetch that changes tests stays far below it.
- Records read or edited by these TOML primitives are capped at **1 MiB**
  (1,048,576 bytes), including user-editable records. Larger input is
  `malformed` with a size-limit reason, before parsing; reads are bounded
  even if a file grows while being read. An edit may not grow past the cap.
- Missing `schema`: user-editable → 1; tool-only → `SchemaMissing`.
- Newer than supported: raw display only; no semantic use, mutation or
  execution based on it. A job directory with newer-schema metadata is
  never pruned (reported).
- Errors: `SchemaMissing`, `SchemaMalformed`, `SchemaTooNew`, `SchemaTooOld`.
- **User-editable record classification** (`problem.toml`, and the other
  user-editable formats when a command reads them): `missing` (no file,
  including a non-directory path component / ENOTDIR);
  `unreadable` (bytes cannot be read, or not a regular file); `malformed`
  (not TOML; `schema` present but not an integer; `SchemaTooOld`; a supported
  schema with a missing or invalid field, e.g. a `problem.toml` without a
  string `[problem] id` that is a valid identity, §2.1); `too_new`
  (`schema` newer than supported: nothing else is decoded); `valid`. An
  absent `schema` means 1. Steps in order: read bytes; parse TOML; read
  `schema` alone; decode and validate fields. Directory binding of a valid
  identity (§2.1) is checked after classification and is not part of it.
- **Record classification** of the tool-only records `state.toml` (§5.7)
  and, *(draft)*, `job-seq`, `facts/*.toml` and the fetch records, one enum used everywhere: `missing` (no file);
  `unreadable` (the bytes cannot be read: I/O error, permission, not a
  regular file); `malformed` (not TOML; `schema` absent or not an integer;
  `SchemaTooOld`, none exists yet; or a supported schema with a missing or
  invalid field); `too_new` (`schema` newer than supported: nothing else is
  decoded); `valid`. Steps in order: read bytes; parse TOML; read `schema`
  alone; only for the supported schema decode and validate the fields. A
  file is never decoded as the current schema before its `schema` is
  checked.
- *(draft)* **Encodings** in these records and in `job.toml`: `epoch` is a
  string of exactly 32 lowercase hex digits (128 bits); `n` and `next` are
  **decimal strings** (TOML integers are signed 64-bit, so a `u64` is not
  stored as one): ASCII digits only, no sign, no leading zero except `"0"`,
  value ≤ `u64::MAX`. Anything else is `malformed`. Tests: `"0"`,
  `"9223372036854775807"`, `"9223372036854775808"`,
  `"18446744073709551614"`, `"18446744073709551615"` (valid; admission then
  refuses with `SequenceExhausted`), `"18446744073709551616"`, `"-1"`,
  `"01"`, `"+1"`, an integer `5` (all `malformed`).

## 10. Allowed states and tests

| Commit point | Crash before it | Crash after it |
|---|---|---|
| atomic replace | old content; temp may remain (recovered §4.5 in metadata recovery locations; inside a job directory, pruned with it) | new content complete |
| shared-record edit (§4.4) | old content; temp may remain | new content complete |
| file no-clobber publish | target absent; temp may remain | target complete (link fallback: temp may remain) |
| dir publish (`add`) | no `problems/<name>`; staging may remain | complete problem; membership may be missing (§5.2 step 5) |
| `test add` | files absent, or published but unregistered (`ls` shows them as candidates) | entry registered |
| job admission (lease locked) | no dir; dir without lease; or dir with a lease file not yet locked (both interrupted, inactive, reclaimable) | job dir with lease: inactive orphan once `cpf` is dead, reclaimable |
| prune rename | job dir intact | `.reclaim-*`, deleted by a later prune |
| *(draft)* `job-seq` replace (admission 2a) | old counter | counter advanced; no job dir yet = an unused number (gap) |
| *(draft)* `job.toml` publish (3a) | job dir without `job.toml`: interrupted admission | job dir with sequence, lease not yet locked: interrupted, reclaimable |
| *(draft)* fact replace (§6.5) | previous fact | new fact, even if the lease was never released |
| *(draft)* fact publication with another epoch or unavailable `job-seq` | previous fact, `NotPublished` reported | — (no commit) |
| `init` `job-seq` (§5.1) | no `job-seq`, no `cpforge.toml`: rerun is initial | `job-seq` without `cpforge.toml`: rerun keeps it |
| `init` `cpforge.toml` (§5.1) | valid `job-seq`, no `cpforge.toml`: rerun keeps the sequence and publishes config | complete workspace; rerun keeps both files |
| *(draft)* `fsck` new epoch | `job-seq` missing or malformed, admission refused | new epoch; jobs of old epochs publish no fact |
| *(draft)* `job-seq` `unreadable` (permission removed) | admission refused naming the error; a finishing job publishes `NotPublished{SequenceUnavailable}`; `fsck --apply` reports, does not repair | after access is restored: admission continues the same epoch |
| *(draft)* fact classification | valid TOML with supported schema but no `n`, or `n = "01"`: `malformed`, replaced on publication; newer-schema fact lacking current fields: `too_new`, preserved by publication and by `fsck` for a deleted problem | — |
| *(draft)* adopt / gitignore | old content | new content |
| *(fetch, draft)* download area (step 0, `fetch.prepared`) | nothing, or `downloads/<txn>/` without `lease`: reclaimed by the next recovery | area with its lease, free once `cpf` died: reclaimed by the next recovery; nothing else changed |
| *(fetch, draft)* network phase (`fetch.downloaded`) | as the row above | as the row above |
| *(fetch, draft)* plan computed (step 2, `fetch.revalidated`) | as the row above | as the row above; no journal, no status |
| *(fetch, draft)* journal publish (step 3, `fetch.journal_published`) | no journal; workspace unchanged | journal, no planned file present: reconciliation rolls forward only a plan without files whose bases match, else rolls back (status `rolled_back`, no history record) |
| *(fetch, draft)* each test-file publish (step 4, `file_noclobber.after_publish#i`) | journal and a prefix of the planned files | journal and a longer prefix: reconciliation rolls back, history `rolled_back` lists the present files as `unreferenced`; they stay as candidates |
| *(fetch, draft)* last test-file publish (`fetch.files_published`) | as the row above | every planned file: rolled forward (steps 5–7) if every base matches, else rolled back as above |
| *(fetch, draft)* catalog commit (step 5, `fetch.catalog_committed`) | as the row above | new tests with old metadata, `fetch_incomplete` `committed`: steps 6–7 rolled forward |
| *(fetch, draft)* metadata edit (step 6, `fetch.metadata_committed`) | as the row above | new metadata, history pending: step 7 rolled forward |
| *(fetch, draft)* history publish (step 7, `fetch.history_published`) | as the row above | record present and kept; status and journal removal rolled forward |
| *(fetch, draft)* status replace (`fetch.status_replaced`) | as the row above | journal removed by reconciliation, nothing else changes |
| *(fetch, draft)* journal removal (`fetch.journal_removed`) | as the row above | transaction complete; the area is reclaimed by the next recovery |
| *(fetch, draft)* area rename (`fetch.area_renamed`) | area with a free lease, reclaimed | `.reclaim-<txn>`, deleted by the next recovery |
| *(fetch, draft)* creation staging (`fetch.staged`) | journal, no staging | no `problems/<p>`; staging removed by recovery; reconciliation: status `rolled_back`, journal removed |
| *(fetch, draft)* creation directory publish (`fetch.problem_published`) | as the row above | complete problem with catalog and metadata; history, status, journal removal rolled forward |
| *(fetch, draft)* abort history (§5.8.8, `fetch.abort_recorded`) | journal and published files: rolled back by reconciliation | record `conflict` or `failed` kept; reconciliation rolls back without a second record |
| *(fetch, draft)* contest member loop (`fetch.member_finished#i`) | members committed so far, membership unchanged | as before; a rerun adds every committed member |
| *(fetch, draft)* contest file creation (`fetch.contest_created`) | no `contest.toml` | `schema = 1` and `[contest]` only: a rerun edits it |
| *(fetch, draft)* membership edit (`fetch.membership_edited`) | committed members not yet listed | every committed member listed in enumeration order |

Coverage assignment (command rows remain assigned to their implementing tasks):

| Rows / transaction | Task coverage |
|---|---|
| atomic replace, shared-record edit, file and directory publication primitives; lock/recovery boundaries | C4b (covered by library helper subprocesses) |
| init, init job-seq (including primitive publication and lock/recovery boundaries) | C4c (covered) |
| add directory + membership (including staging, publication, contest creation/edit and lock/recovery boundaries) | C4d (covered) |
| test add, register, enable/disable (§5.3) | C6 |
| job admission, prune, job-seq replace, job.toml, fact replace/publication/classification, unreadable job-seq | C10/C15 tasks |
| fsck new epoch; adopt; gitignore | their respective command tasks |
| *(fetch, draft)* every fetch row above, reconciliation, download-area reclamation, readers, records | the fetch command tasks (transaction and recovery; creation; refetch pair rules; contest membership; status surfaces) |

Reruns: `init`, `add --contest` complete the operation; `test add` may
create a new number and leave the earlier files unregistered. *(fetch, draft)*
A rerun of `cpf fetch` reconciles first (§5.8.7), then fetches; a rerun of a
contest fetch completes its membership.

**Tests:**

- Kill a real subprocess at every commit point (test-only env var); assert
  the allowed states and the rerun behaviour. Storage primitives and edit
  transactions use a library helper subprocess (C4b). Each command task
  additionally kills the actual `cpf` binary at that command's commit points.
- Injected failures: link + failed unlink; rename + failed directory sync
  (`Durability::Uncertain`, caller continues); directory `RENAME_NOREPLACE`
  unsupported; `EXDEV` on publish; existing empty destination directory;
  disk full; permission denied; temp removal failure before commit.
- Edit transaction: in-place edit, atomic editor save, deletion, type change
  and malformed TOML between steps; Windows sharing retries with a save
  between attempts; two `cpf` processes on one file; hard link refused; mode
  preserved; untouched-item round-trip.
- Composition: nested operations never re-acquire the lock; leftover recovery
  runs before the current transaction creates files.
- `test add`: `.ans` `Exists` after `.in` published, and an editor modifying
  the `.in` before that: the `.in` is kept, reported unregistered; a
  registered entry whose `tests/custom-1.in` is missing makes `add` choose
  `custom-2`; an editor taking the id or a path during the replay → conflict,
  catalog unchanged.
- Test commands (C6): `register` uses an existing sibling `.ans` and says
  so, `--no-answer` ignores it, the stem default id and `--id` when the stem
  is taken or invalid; `register` of a path already named → refused;
  `enable`/`disable` after an editor reordered entries changes the right one,
  after the entry was removed reports it and changes nothing, an unchanged
  state writes nothing; `ls` leaves the workspace byte-identical (no lock
  file created) and lists candidates and strays; a malformed or `too_new`
  catalog refuses every mutation and `ls` reports it; absent, empty and
  missing answers; a FIFO in place of an input is refused without blocking
  (Unix), including one swapped in after inspection; unregistered
  candidates cannot be enabled or selected.
- Leases and pruning: held lease never pruned; killed job becomes prunable;
  directory without lease reclaimed; unreadable lease left and reported;
  rename refused (Windows: a handle open inside the job directory; injected
  elsewhere) leaves the directory; `.reclaim-*` leftovers retried; the
  probe is released before the rename (Windows: the rename succeeds with no
  handle open and fails, reported, with the owner's lease open); `keep_jobs` and byte-target triggers;
  newer-schema job metadata never pruned.
- Retention: per-job budget refusal (execution contract); `CwdOverflow`;
  `BuildOutputTooLarge` removes the build directory; a mount point inside a
  job directory is skipped by measuring and deletion.
- Shared `cwd`: never measured or cleared; every file-I/O problem, including
  one with shared `cwd`, is rejected (configuration contract §6.2).
- Freeze: edits between tests and between repeat iterations do not change
  later runs; an edit and an injected interruption **during** freezing (the
  run does not start; the manifest is never partial); freeze failure starts
  no run; FIFO refused.
- Admission: a second `cpf` pruning concurrently never selects a job being
  admitted (lock held through lease creation); `cpf clean` racing admission.
- Mount boundaries: a bind mount of the same filesystem inside a job
  directory (Linux, privileged CI step) is skipped by measurement and
  deletion.
- Per-job accounting: reservation before each copy, replacement temp counted,
  release on removal, overflow of checked arithmetic refused.
- Links in tool-written paths refused; root via a link works.
- Schema refusal per format; answer absent vs present-empty.
- *(fetch, draft)* **Fetch harness.** A test adapter (`storage-test-hooks`)
  returns a given parsed problem or contest (§5.8.1) without network; every
  fetch test below drives the actual `cpf` binary with it. Assertions read
  back, after each step: the catalog (entries in order, `download` tables,
  `[fetch]`), the `problem.toml` tool fields, the files of `tests/` with
  their SHA-256, each history record (outcome, pairs, files and roles), the
  status outcome and findings, whether a journal exists, and the entries of
  `downloads/`. Crash points: `fetch.prepared`, `fetch.downloaded`,
  `fetch.revalidated`, `fetch.journal_published`, `fetch.files_published`,
  `fetch.catalog_committed`, `fetch.metadata_committed`,
  `fetch.history_published`, `fetch.status_replaced`,
  `fetch.journal_removed`, `fetch.area_renamed`, `fetch.staged`,
  `fetch.problem_published`, `fetch.abort_recorded`,
  `fetch.member_finished`, `fetch.contest_created`,
  `fetch.membership_edited`, and `file_noclobber.after_publish#i` during
  step 4; `fetch_reconcile.catalog_committed`,
  `fetch_reconcile.metadata_committed`, `fetch_reconcile.history_published`
  inside reconciliation. Pause barriers: `fetch.network` (area held, no
  lock), `fetch.before_rehash` (files published, before the catalog edit's
  first validation), `fetch.after_rehash` (after the last step-5 rehash,
  before the rename), `fetch.before_metadata`.
- **Fetch crash states:** a kill at each crash point above for a creation, a
  refetch with one `updated`, one `added` and one `kept_edited` pair, and a
  metadata-only refetch; reconciliation then by `cpf fetch` (same parsed
  problem) and, separately, by `cpf fsck --apply`; the recovered state
  equals the §10 row; a second reconciliation changes nothing; no test file
  is ever modified or deleted (hashes of every pre-existing file unchanged).
  A kill inside reconciliation at each `fetch_reconcile.*` point, then a
  further reconciliation, gives the same final state.
- **Restart cases** (after the kill, before reconciliation): `tests.toml`
  edited (an `enabled` change) → rolled back; a planned file modified →
  that file is not in the history record and the others are `unreferenced`;
  a planned file name created by the user with other bytes → user content,
  not recorded; a `problem.toml` tool field edited after a kill past step 5
  → kept, `metadata_conflict{field}`; the catalog edited after step 5 →
  steps 6–7 still rolled forward, catalog untouched by reconciliation.
- **Aborts:** `Exists` on a planned name (created at `fetch.before_rehash`
  by another process) → `name_taken`, history `failed`, earlier files
  `unreferenced`; a hash mismatch injected into the area → `failed`, the
  file not published; an editor save of a replaced input at
  `fetch.before_rehash` → `conflict` `edited_during_commit`, catalog
  byte-identical, published files candidates; the Windows retry path with a
  save between attempts → the same conflict.
- **Observation points and the race:** a save of a download entry's input at
  `fetch.network` → `base_changed`, nothing published, no journal; a save at
  `fetch.after_rehash` → the catalog names the revision pair, the superseded
  file keeps the user's bytes, `cpf status` reports it as edited (the
  stated race); a save to `problem.toml` at `fetch.before_metadata` →
  per-field rule.
- **Pair rules:** one test per row of the §5.8.5 outcome table, each with an
  enabled and a disabled entry; input-only edit, answer-only edit, deleted
  input, deleted answer, input replaced by a directory; each ambiguity kind
  (`duplicate_ordinal`, `duplicate_recorded`, `duplicate_upstream`, `moved`
  by swap and by insertion at the front) → conflict, workspace
  byte-identical except `status.toml`; identical upstream with duplicate
  pairs → `unchanged`; zero samples with and without `--disable-removed` →
  set kept, `zero_samples`; `removed_upstream` with `--disable-removed` →
  disabled, files untouched; `removed_locally` → not re-added; three
  refetches with an edited pair and an unchanged upstream revision → exactly
  one unregistered upstream pair; an unchanged refetch → every file
  byte-identical except `status.toml`, no history record; three refetches
  that each update a pair, a restart, then `cpf test register` of a
  superseded file → registered as `custom`, history records unchanged, and
  provenance `registered` before `downloaded`.
- **Names:** `sample-1.r2.in` present and unregistered → `r3`; a directory
  named `sample-1.r2.ans` → `r3`; a catalog path naming a missing
  `sample-1.r2.in` → `r3`; a history-listed name deleted by the user is not
  reused; `added` on an existing unregistered `sample-1.in` → `sample-1.r2`
  with id `sample-1.r2`; collision-key variants (`Sample-1.R2.in`) count as
  taken.
- **Download area:** at `fetch.network`, `cpf fsck --apply`, `cpf clean
  --apply` and `cpf clean` from other processes leave the area and its
  lease intact and the fetch then commits; a kill at `fetch.network` → the
  next locked command removes the area; an area without `lease` → removed;
  an unopenable lease (Unix permission) → kept and reported by `fsck` and
  as `RecoveryIncomplete`; an unexpected entry in `downloads/` → kept and
  reported; the owner's own recovery at step 2 sees its area `held`;
  Windows: owner release renames with no handle open.
- **Concurrency:** two fetches of one problem paused at `fetch.network`,
  released in turn → the first commits, the second `base_changed` with
  nothing published; a rerun → `unchanged`. A fetch and `cpf test add` on
  one problem serialize by the lock; a `test add` between steps 0 and 2 →
  `base_changed`.
- **Records:** each fetch record in each classification (`missing`,
  `unreadable`, `malformed`, `too_new`, `valid`): a `too_new` journal
  blocks `cpf fetch` and survives `fsck --apply`; a `malformed` journal is
  removed by `fsck --apply` and its files become `unknown`; a `too_new`
  status is not replaced; a `too_new` or `malformed` history record
  contributes no provenance; a history record of another identity
  contributes none; download-field validation (`download` on `custom`;
  without `answer`; a missing key; `ordinal` 0 and 65; uppercase or short
  hash; invalid `txn`; `[fetch]` without `txn`) → `malformed`; a version
  without fetch support preserves the fields as unknown keys through `test
  enable`; a plan whose catalog would pass 1 MiB → `record_too_large`,
  nothing published.
- **Readers:** with a `pending` and with a `committed` journal, `cpf status`
  and `cpf test ls` report `fetch_incomplete` with that phase and leave the
  workspace byte-identical; a pending journal's files are candidates and
  never run.
- **Metadata:** an unavailable field is not written; a limit change with a
  contest record `limits` override → only `problem.toml` changes,
  `contest.toml` byte-identical.
- **Creation:** an existing local problem whose name is the derived one →
  refused, nothing created; a directory bound to another remote identity →
  `IdentityMismatch`; zero samples → empty catalog with `upstream_samples =
  0` and `no_samples`; non-ASCII sample bytes published exactly.
- **Contest membership:** a kill at `fetch.member_finished#2` of four, then a
  rerun → membership equals the enumeration in order; a member failing in
  the rerun but committed earlier → listed; a member never committed → not
  listed; insertion before the first later member; an existing record with
  another label keeps it; a label held by another record → record without
  label, reported; `[contest] id` mismatch → refused; a problem shared by
  two contests is one directory listed in both.
- **fsync order** (trace recorder, `storage-test-hooks`): for every fetch
  publication, file sync before rename before directory sync; the journal's
  directory sync before the first test-file publication; the last test
  file's directory sync before the catalog rename; the journal unlink
  followed by its directory sync. Linux and macOS record directory syncs;
  Windows records file flushes before each rename.
- Every fetch test runs on Linux, macOS and Windows (fetch starts no
  process).
- Real runs on Linux, macOS and Windows on the supported filesystems;
  power-loss behaviour is not tested. Platform floor and toolchain as in
  [README](../../README.md) (platform floor).
