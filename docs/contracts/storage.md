# Storage contract

Status: v3. Normative for the `workspace` module and the storage side of
`jobs`. Initial scope: manual problems and tests, job directories, retention.
Fetch, generation-based publication and archives are planned for later
versions; the initial implementation reaches tests only through the
test-store interface (§8).

**Managed-workspace amendment (v5): settled, implementation in progress.** The
single-workspace rows of `../design-workspace.md` §9 cover admission sequence,
source capture, run facts, adopt / clean / fsck / gitignore transactions.
Amended content is marked *(draft)* below; init sequence bootstrap is implemented
(C4c), while the remaining amended transactions are planned.
References labelled *workspace design* point to `../design-workspace.md`.

Related: design §4 and `execution.md`.

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
  containing `cpforge.toml` (`--workspace` overrides); it may be reached
  through a link and is canonicalised once.
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
  a shared `cwd` are rejected in the CLI foundation.

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
  no-clobber publication that returns `Exists` although rule 1 applied
  (concurrent creation outside cpf, or an alias the key missed) is
  `NameCollision{reason: NativeExists}`, never `Exists`.
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

Tool fields (`id`, `url`, `kind`, published limits, `schema`) and user fields
(tags, status, language, comparator, comments, unknown keys) share a TOML
file. A command changes only its own fields; untouched items survive as a
`toml_edit` round-trip. Permanent user files (sources, tests, notes) are
outside every retention budget.

**Reserved trees** *(draft)*: `.cpforge/runs/`, `.cpforge/cache/`,
`.cpforge/staging/`, `.cpforge/facts/` and the files `.cpforge/state.toml`,
`lock`, `job-seq` belong to cpf in their entirety, whoever created an entry.
Only `runs/` (by §6.3 pruning and the job's own §6.4 cleanup) and `cache/`
(initial feature release) are disposable. No path under `problems/` or `contests/` is
disposable; outside the reserved trees nothing is deleted automatically,
with one exception: leftover recovery (§4.5) removes regular files named
exactly `.cpf-tmp-` + 32 lowercase hex digits in its metadata recovery
locations (a reserved name, §2.1).
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
3. Write the temp file (§4.2 step 1).
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
   failed: <reason>", partial-failure exit code (report transport); rerunning
   `cpf add --contest` completes it.

### 5.3 `cpf test add`

1. Lock. Next `n` with neither `custom-<n>.in` nor `custom-<n>.ans` present.
2. `publish_file_noclobber` the `.in`, then the `.ans` if any.
3. Edit transaction on `tests.toml` adding the entry with its hashes.
4. **Published files are never deleted.** If the `.ans` publish fails
   (`Exists` from a concurrent creator, I/O error) or step 3 fails, the
   published files stay, discovery shows them as *unregistered*, and the
   command reports the partial result. A retry may choose a new `n`.

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
`.cpforge/staging/`, then the comment `# optional: .cpforge/facts/`.
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

Never user files, never `.cpforge/lock`, never lock abandonment inferred
from age or PIDs.

## 6. Job directories, leases, pruning

### 6.1 Layout

```
.cpforge/runs/<job-id>/            time-ordered + random id, never reused
    lease                          lock file held by the job owner
    job.toml                       (draft) schema, (epoch, n) admission sequence
    report.json
    inputs/<test-id>.in|.ans       frozen copies (§7)
    inputs/src/...                 (draft) captured solution-target sources (§7)
    builds/<build-id>/             fresh output directory per build (build planning)
    <run-id>/  spec.json record.json cleanup.log  out/stdout out/stderr  cwd/
.cpforge/runs/.reclaim-<job-id>/   being deleted
```

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
service's transferred runs (execution contract §3.3) and the report writer hold clones. The
lease is released when the last clone drops.

**Inactive** = no cooperating owner holds the lease (the job finished or its
`cpf` died). A job directory **without a `lease` file** is an interrupted
admission and inactive; a lease that exists but cannot be opened
(permission, I/O error) leaves the directory in place and is reported.

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
   directories oldest first. Candidates: beyond the newest `keep_jobs`
   (*default* 20) inactive jobs; then, using **projected** usage (measured
   usage minus the measured size of candidates already selected), further
   oldest inactive jobs while projected usage exceeds `workspace_bytes`
   (*default* 4 GiB). Measurement is `known bytes + incomplete: bool`; an
   incomplete measurement selects by `keep_jobs` only and is reported.
2. For each candidate: try-lock its lease with a probe handle (held → skip;
   absent → interrupted admission, proceed); rename the directory to
   `runs/.reclaim-<job-id>` while holding the probe lock. A failed rename
   leaves the directory in place (reported, retried next time).
3. Release probe locks and the workspace lock; delete each `.reclaim-*` with
   the no-follow walker (§2). Leftovers stay as `.reclaim-*` and are retried by the
   next prune.

### 6.4 Retention

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
  into `cwd/`, specs, records, reports, logs, and their temp files during
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
- **Publication** at job finalization of a job that **completed** (not
  cancelled, not failed before its report), **before** the lease is
  released: lock; read `job-seq`, then the existing fact; decide by the
  first matching row; release the lock.

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
  the job's exit code (report transport). A job cancelled or interrupted before the
  commit leaves the previous fact unchanged; `status` calls it the last
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
  the lock: tests (above), then the selected solution target's source files
  (build planning target resolution) copied into `inputs/src/` with their
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
    fn add(&mut self, g: &WorkspaceGuard, input: &[u8], answer: Option<&[u8]>, origin: Origin) -> Result<AddOutcome, StorageError>;
    fn set_enabled(&mut self, g: &WorkspaceGuard, id: &TestId, enabled: bool) -> Result<(), StorageError>;
    fn freeze_into(&self, ids: &[TestId], job: &JobInputs) -> Result<FrozenManifest, StorageError>;
}
```

`TestEntry` distinguishes answer absent / present-empty / present and
registered / unregistered; `AddOutcome` reports partial results (§5.3).
The CLI foundation implements it over `tests.toml` + `tests/`; the initial feature release may use
generations behind it. Lock-free readers of metadata tolerate absence,
replacement between reads and malformed hand edits (reported with location);
captures, logs and build outputs are running artefacts until their job
finalizes them (execution contract §6.2).

## 9. Schema versions

- Governed formats: `cpforge.toml`, `problem.toml`, `tests.toml`,
  `contest.toml` (user-editable); `state.toml`, `spec.json`, `record.json`,
  `report.json`, *(draft)* `job-seq`, `job.toml`, `facts/*.toml` (tool-only).
- Missing `schema`: user-editable → 1; tool-only → `SchemaMissing`.
- Newer than supported: raw display only; no semantic use, mutation or
  execution based on it. A job directory with newer-schema metadata is
  never pruned (reported).
- Errors: `SchemaMissing`, `SchemaMalformed`, `SchemaTooNew`, `SchemaTooOld`.
- **User-editable record classification** (`problem.toml`, and the other
  user-editable formats when a command reads them): `missing` (no file);
  `unreadable` (bytes cannot be read, or not a regular file); `malformed`
  (not TOML; `schema` present but not an integer; `SchemaTooOld`; a supported
  schema with a missing or invalid field, e.g. a `problem.toml` without a
  string `[problem] id` that is a valid identity, §2.1); `too_new`
  (`schema` newer than supported: nothing else is decoded); `valid`. An
  absent `schema` means 1. Steps in order: read bytes; parse TOML; read
  `schema` alone; decode and validate fields. Directory binding of a valid
  identity (§2.1) is checked after classification and is not part of it.
- *(draft)* **Record classification** of the tool-only records `job-seq`
  and `facts/*.toml`, one enum used everywhere: `missing` (no file);
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
| `test add` | files absent, or published but unregistered | entry registered |
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

Coverage assignment (command rows remain assigned to their implementing tasks):

| Rows / transaction | Task coverage |
|---|---|
| atomic replace, shared-record edit, file and directory publication primitives; lock/recovery boundaries | C4b (covered by library helper subprocesses) |
| init, init job-seq (including primitive publication and lock/recovery boundaries) | C4c (covered) |
| add directory + membership | C4c (pending) |
| test add | C6 |
| job admission, prune, job-seq replace, job.toml, fact replace/publication/classification, unreadable job-seq | C10/C15 tasks |
| fsck new epoch; adopt; gitignore | their respective command tasks |

Reruns: `init`, `add --contest` complete the operation; `test add` may
create a new number and leave the earlier files unregistered.

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
  the `.in` before that: the `.in` is kept, reported unregistered.
- Leases and pruning: held lease never pruned; killed job becomes prunable;
  directory without lease reclaimed; unreadable lease left and reported;
  rename refused (Windows open handle without share-delete, injected
  elsewhere) leaves the directory; `.reclaim-*` leftovers retried; Windows
  rename with an open lease; `keep_jobs` and byte-target triggers;
  newer-schema job metadata never pruned.
- Retention: per-job budget refusal (execution contract); `CwdOverflow`;
  `BuildOutputTooLarge` removes the build directory; a mount point inside a
  job directory is skipped by measuring and deletion.
- Shared `cwd`: never measured or cleared; file-I/O problem with shared `cwd`
  rejected.
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
- Real runs on Linux, macOS and Windows on the supported filesystems;
  power-loss behaviour is not tested. Platform floor and toolchain as in
  [README](../../README.md) (platform floor).
