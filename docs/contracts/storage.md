# Storage contract

Status: v5. Normative for the `workspace` module and the storage side of
`jobs`: problems and tests, job directories, retention, and the
managed-workspace transactions of `../design-workspace.md` §9 (admission
sequence, source capture, run facts, adopt / clean / fsck / gitignore),
which are implemented. Jobs reach tests only through the test-store
interface (§8). References labelled *workspace design* point to
`../design-workspace.md`.

**Gate-2 additions** (approved; implemented by the gate-2 tasks). *Fetch*: remote identities (§2.1), the fetch
transaction and test publication (§5.8), the download area and fetch
records (§3, §4.5), catalog download fields (§8.1), their schemas (§9) and
allowed states (§10); it replaces generation-based publication: tests are
published through the catalog commit of §5.8. *Organisation*: tags,
workflow status, dates and notes, `cpf ls` and `cpf search` (§5.9), their
ownership rows (§3), field rules (§9) and allowed states and tests (§10).
*Stress*: saved failures under `problems/<p>/stress/failures/` (§3
ownership, §9 `failure.toml`), `cpf test add --from-stress` (§5.3), the
gitignore comment (§5.5), `fsck` recognition (§5.6), the stress job layout
(§6.1), byte release after confirmed removal and at promotion (§6.4),
stress inputs (§7) and the crash rows (§10); the transactions themselves
are in the stress contract (`stress.md` §§6–8). *Archive*: archive
transactions and the quarantine (§3), their recovery (§4.5), `fsck`
(§5.6), fetch records moving with their problem (§5.8.10), archived
listing and search (§5.9.6–§5.9.8), records (§9) and allowed states (§10);
the transactions themselves are specified in `archive.md` (archive
contract). *Submission*: the submission tree `.cpforge/submit/` (versions
and `current`), the job's `submit/` entries and the external test
directory record: ownership (§3), recovery (§4.5), gitignore (§5.5), fsck
(§5.6), job layout and pruning (§6.1, §6.3), the artifact-test guard (§7),
formats (§9) and commit points (§10); the protocols are in the submission
contract (`submit.md`).

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
  anything in it; the record marks `shared_cwd`; a file-based I/O problem
  with a shared `cwd` is a configuration error (configuration contract
  §6.2); file-based I/O always runs in the run's own `cwd/` (execution
  contract §5.6).

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
- **Remote identity**: `<site>:<native>` with `site` from
  the general grammar and not `local`, and `<native>` from that site's
  native-id grammar (sites contract). The directory is `<site>-<native>`,
  which must be an Identifier; an identity whose directory name is not one
  is refused by its adapter. `problems/<site>-<native>/problem.toml` must
  hold exactly that identity (same spelling); anything else is
  `IdentityMismatch`. The derivation need not be injective: a directory
  holds one identity, and a second identity deriving the same name (a
  local `atcoder-abc400_a`, or another site) is refused, never mapped to
  it. Settled: `cpf add` does not reserve remote directory forms; the later
  fetch refuses (§5.8.4 step 0). The resolution rules for `cpf add` apply to fetch creation with the
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
| `problems/<p>/notes.md` | user | created empty, no-clobber, under the lock, only by `cpf notes` (§5.9.5); never modified, truncated, renamed or deleted |
| `problem.toml` `[problem]` `tags`, `status`, `created_at`, `updated_at`, `solved_at`; `contest.toml` top-level `tags` | user fields | edit transaction by `cpf tag`/`untag` (`tags`, `updated_at`), `cpf mark` (`status`, `updated_at`, `solved_at`); `created_at` only in the staged record of a creating command (§5.2, §5.8.4); never another command (§5.9.1) |
| `problem.toml` `[problem]` `archived_at` | user field | the archive contract only; read by §5.9 readers. Set only in a snapshot's staged copy (archive contract §5.2); an active problem carries it after a restore |
| `problem.toml` `[problem]` `origin` | tool field | written only by `cpf restore --into` (archive contract §7.4) |
| `.cpforge/archive/<txn>/` | tool, leased | created with its journal and lease under the lock; holds the retired trees of one transaction; removed by its transaction, its recovery or `cpf archive --close` (archive contract §5.3, §6) |
| `.cpforge/quarantine/<txn>/` | tool tree holding user content | entries moved in by archive cleanup; never modified; removed only by `cpf quarantine purge --apply` (archive contract §6.6) |
| `.cpforge/state.toml` | tool | atomic replace under the lock |
| `.cpforge/runs/<job>/` | tool, disposable | by its job (§6) |
| `.cpforge/staging/` | tool | only while holding the workspace lock |
| `.cpforge/lock` | tool | lock only; never unlinked or replaced |
| `.cpforge/job-seq` | tool | atomic replace under the workspace lock (§6.2) |
| `.cpforge/facts/<problem>/<target>.toml` | tool | atomic replace under the workspace lock (§6.5) |
| `problems/<p>/problem.toml` `[files] user` | user field | edit transaction by `cpf adopt` (§5.4) |
| `problems/<p>/problem.toml` `[files] created` | tool field | edit transaction by the creating command |
| `<root>/.gitignore` cpf block | tool block in a user file | only on `cpf gitignore --write` (§5.5) |
| `.cpforge/downloads/<txn>/` | tool, leased | created with its lease under the lock, written lock-free only by the lease owner; removed by the owner or by recovery when the lease is free (§5.8.3) |
| `.cpforge/fetch/<p>/journal.toml` | tool | no-clobber under the lock, atomic replace at its metadata phase; removed when its transaction finishes or is reconciled (§5.8.7) |
| `.cpforge/fetch/<p>/txn-<txn>.toml` | tool, durable | no-clobber under the lock; never rewritten or removed by cpf (§5.8.10) |
| `.cpforge/fetch/<p>/status.toml` | tool | atomic replace under the lock (§5.8.9) |
| `problems/<p>/tests.toml` `[fetch]`, entry `download`; `problem.toml` `[problem]` `url`, `title`, `kind`, `limits`; `contest.toml` `[contest]` | tool fields | edit transaction by `cpf fetch` (§5.8), never another command |
| `problems/<p>/stress/`, `problems/<p>/stress/failures/` | user once created | created with `create_dir` under the lock by saved-failure promotion when absent (stress contract §7.5); never removed or renamed; other entries of `stress/` are user files cpf ignores |
| `problems/<p>/stress/failures/<id>/` | user once published | published complete by `publish_dir_noclobber` under the lock (stress contract §7.5); **never modified, renamed or deleted** by cpf; read only as untrusted input (stress contract §7.6) |
| `.cpforge/submit/<p>/<vid>/` | tool, immutable | published by a no-clobber directory rename under the lock (submission contract §5.3, §6.7); never modified; removed only by `cpf clean` when unreferenced (submission contract §11.1) or moved by archive retirement |
| `.cpforge/submit/<p>/current` | tool | atomic replace under the lock, right after the version it names was published by the same operation; never removed by cpf |
| `.cpforge/runs/<job>/submit/` | tool, disposable with its job | written by its `submit` job under its lease (`out/`, `version/`, `external.toml`); the external test directory it records is removed by the job or by pruning (§6.3) |

Tool fields (`id`, `url`, `kind`, published limits, `schema`) and user fields
(tags, status, language, comparator, comments, unknown keys) share a TOML
file. A command changes only its own fields; untouched items survive as a
`toml_edit` round-trip. Permanent user files (sources, tests, notes) are
outside every retention budget.

**Reserved trees**: `.cpforge/runs/`, `.cpforge/cache/`,
`.cpforge/staging/`, `.cpforge/facts/`, `.cpforge/downloads/`,
`.cpforge/fetch/`, `.cpforge/archive/`,
`.cpforge/quarantine/`, `.cpforge/submit/` and the
files `.cpforge/state.toml`,
`lock`, `job-seq` belong to cpf in their entirety, whoever created an entry.
`.cpforge/cache/http/` is the disposable HTTP cache of the sites contract
§6, within `cache/`.
Only `runs/` (by §6.3 pruning and the job's own §6.4 cleanup) and `cache/`
(initial feature release) are disposable; a `downloads/` entry is removed
only by its protocol (§5.8.3), and `fetch/` is not disposable; under `submit/` only unreferenced versions are removed, by `cpf
clean` (submission contract §11.1). No path under `problems/` or `contests/` is
disposable (saved failures included: they are user work,
never pruned and outside every retention budget); outside the reserved trees nothing is deleted automatically,
with one exception: leftover recovery (§4.5) removes regular files named
exactly `.cpf-tmp-` + 32 lowercase hex digits in its metadata recovery
locations (a reserved name, §2.1). Fetch never deletes or modifies a file
under `problems/<p>/tests/` (§5.8).
**Archive exception.** "Never modified or deleted" and
"never renamed" (sources, tests, `notes.md`, fetch history records) have
one exception: `cpf archive --apply` renames a whole problem or contest
directory, and the problem's `.cpforge/fetch/<p>/`, into
`.cpforge/archive/<txn>/` after its snapshot is committed, and cleanup then
moves each file to the OS trash or the quarantine, or deletes it, by the
archive contract's revalidation rules (archive contract §5.3, §6). Neither
`archive/` nor `quarantine/` is disposable.
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
  A workspace guard publishes directories from
  `.cpforge/staging/` and, for restore, from `.cpforge/archive/<txn>/`
  (archive contract §7.3 step 6); an archive root has its own guard and
  primitives (archive contract §4.1), never a workspace guard.
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

**Prepared form** (used by fetch step 6, §5.8.4, and by its
step-2 size preflight). `prepare` runs steps 1–3 once and returns, without
writing anything: the full baseline of step 1 (bytes, length, SHA-256,
identity, file type, hard-link count), and either *unchanged* or the
**exact output bytes** that would be published (after the formatting and
line-ending preservation of step 2, i.e. the bytes step 5 writes, not a
re-rendering). Size checks and any hash of the result use those bytes.
`publish(prepared)` then performs steps 3 (temp), 4 and 5 with exactly those
bytes: every validation (also every Windows retry) compares against the
**retained** baseline, never a fresh one, and nothing is re-rendered.
Dropping a prepared edit writes nothing (its temp exists only inside
`publish`). `publish` of an *unchanged* result is not allowed (the caller
returns as step 3 does). Outcomes and cleanup are those of steps 4–5: a
`Conflict` removes the temp (or reports `Cleanup::Pending`) and returns;
there is no built-in replay, and a caller that retries calls `prepare` again
(new baseline, new bytes). `check` and the operations stay replayable and
side-effect free; anything the caller does between `prepare` and `publish`
(fetch's journal replacement) is the caller's, outside the callbacks. The
**expected-hash** variant (fetch step 5) compares the step-1 SHA-256 with a
given hash before applying any operation and returns `Conflict` on a
difference; it is used without replay.

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
  each `.cpforge/facts/<p>/`), non-recursively, only regular files
  named exactly `.cpf-tmp-` + 32 lowercase hex digits. The `.cpf-` prefix is
  reserved (§2.1), so no name cpf validates can match; a matching file
  created outside cpf is removed (documented, §3). Temporary files inside a
  job directory (e.g. from publishing `job.toml`, §6.2) are not recovered
  here: they go with the job directory when it is pruned (§6.3).
  Each `.cpforge/fetch/<p>/` is a metadata recovery
  location too. Recovery also reclaims download areas whose lease is free
  and `.reclaim-<txn>` leftovers in `.cpforge/downloads/` (§5.8.3); a held
  or unprobeable area is kept. Every locked operation does this, `cpf
  clean` plan mode included. Recovery never reconciles fetch journals
  (§5.8.7).
  Each `.cpforge/archive/<txn>/` is a metadata recovery
  location too; every record publication there happens under the
  workspace lock (archive contract §6.2), so a temporary file found by this
  recovery never belongs to a live operation. After the steps above,
  recovery deletes `.cpforge/quarantine/.purge-*` leftovers and applies the
  archive contract's rule R (archive contract §6.7): it removes
  initialization leftovers, finishes teardowns, rolls `retiring`
  retirements and interrupted restores forward or back by object identity,
  renames only cpf's side trees, replaces `state.toml`, deletes facts or a
  dead restore's copies, and never moves a user file. Archive cleanup
  (trash, quarantine, delete) is never part of this recovery.

  Each `.cpforge/submit/<p>/` is a metadata recovery
  location too, and recovery deletes its `.reclaim-<vid>` entries (with the
  no-follow walker, after the lock is released, as pruning does). It never
  removes a version or `current`.
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
   The staged `problem.toml` carries `[problem]
   created_at` (§5.9.2 timestamp, the time of this step); it is published
   with the directory, so no state has a problem without it.
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

**`cpf test add --from-stress <id>`** (stress contract
§2.4). Instead of `--input` and `--answer`, the sources are the saved
failure's `input` and, iff its manifest has an `answer` key, the file it
names (stress contract §7.3). Before the lock: classify the failure
(stress contract §7.6; not `valid` → refused, exit 8, nothing written),
then capture each source into a private temporary file as above while
computing its length and SHA-256; a mismatch with the manifest, a link or
a non-regular file → refused, exit 8, the captures removed. Then steps
1–4 above with `stress-<n>` in place of `custom-<n>` (the same smallest-`n`
and collision rules, over the keys of `stress-<n>`, `tests/stress-<n>.in`
and `.ans`) and the entry `origin = "stress"`. The failure directory is
only read.

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

### 5.4 `cpf adopt <path>`

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

### 5.5 `cpf gitignore [--write]`

Prints exactly these lines: `.cpforge/runs/`, `.cpforge/cache/`,
`.cpforge/staging/`, `.cpforge/downloads/`, `.cpforge/submit/`, `.cpforge/archive/`,
`.cpforge/quarantine/` (archive contract §9), then the comment `# optional: .cpforge/facts/`
and the comment `# optional: .cpforge/fetch/` (settled:
never an active line; fetch history is the provenance of downloaded test
files, and ignoring it makes them user content in another checkout, which is
safe), and the comment `# optional: problems/*/stress/failures/`
(never an active line: saved failures are user work that a user may want
to commit, but they can be large and hold platform-specific programs).
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

### 5.6 `cpf fsck [--apply]`

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

`fsck` also reports: fetch journals (`fetch_incomplete`,
§5.8.12) and their classification; held and unprobeable download areas;
unexpected entries in `downloads/` and `fetch/`; non-`valid` history and
status records; history records whose identity differs from the
problem's; fetch records of problems that no longer exist (reported only,
never removed: history is provenance); journals left by §5.8.7 rules 3–4
(`history_blocked`, `undetermined`) and `fetch_inconsistent`, naming the
record the user must repair or move. `--apply` reconciles every `valid`
journal (§5.8.7) and removes `malformed` journals; it never removes a
download area other than through recovery's lease probe, never a history
or status record, never a test file.

`fsck` also reports, per problem, the entries of
`stress/failures/` that do not classify `valid` (stress contract §7.6:
`unexpected`, `missing`, `unreadable`, `malformed`, `too_new`, with the
reason), and a `stress` or `stress/failures` path that is not a directory
or is a link. It verifies no file hash (that is done at use) and
`--apply` never repairs, renames or removes anything under
`problems/<p>/stress/`: saved failures are user work. An id whose epoch
is not the current `job-seq` epoch is valid (ids stay distinct across
sequence repair, stress contract §7.1) and not reported.

`fsck` also reports open archive transactions (kind,
phase, lease state, incomplete items, non-`valid` journals), quarantine
entries by state (`ready`, `pending`, `record_missing`, record
classification), `.purge-*` leftovers, unexpected entries in `archive/`
and `quarantine/`, and an `[problem] origin` that is not an identity
(archive contract §9). `--apply` does only what every locked operation's
recovery does for these trees (rule R); it never moves, trashes or deletes
a retired or quarantined file.

`fsck` also reports the submission tree and job
directories with an external test directory record (submission contract
§11.2, §7.4); `--apply` changes nothing under `.cpforge/submit/` and never
removes an external test directory.

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
is selectable by any source. Two further forms resolve to that same
lookup:

- a **remote identity** `<site>:<native>` (§2.1) names the directory
  `<site>-<native>`, which must hold exactly that identity; a reference
  whose site or native part is outside its grammar is an invalid
  reference;
- **`<contest>/<label>`**: `<contest>` is an Identifier naming an entry of
  `contests/`, found by the same rules as an entry of `problems/` (exactly
  one entry with the key, spelled exactly as given, a directory and not a
  link); its `contest.toml` must classify `valid`; exactly one
  `[[problem]]` record must carry `<label>` (exact spelling, §5.2), and
  that record's `id` is then resolved as an identity reference. Two
  records with one label make `contest.toml` `malformed` (§5.2), so an
  ambiguous label is that error. A label never falls back to a problem
  name or to another contest.

Every failure (an unknown contest, a contest entry or `contest.toml` that
is not valid, no record with the label, the named problem missing or
invalid) is an error naming the reference and the cause (failure class
`workspace`, exit 8). Path references are later designs. **Readers** (`current`, and every command that only resolves
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
current directory is in A uses B's `[config] language`; remote and
`<contest>/<label>` references (§10 SG-36).

### 5.8 `cpf fetch`: fetch transaction and test publication

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
- **Selection**: what one `cpf fetch` resolves before any problem
  transaction, exactly one of
  - `Problem(parsed problem)`;
  - `Contest { identity, url, title (optional), members }`: a site contest
    (AtCoder in gate 2); `members` is the ordered list of `(label, problem
    identity)`, labels from the contest record label grammar (§5.2). Only
    this variant writes membership (step 8, §5.8.13);
  - `ProblemSet { site, section, members }`: one section of the CSES
    problemset (`site = "cses"`, `section` as the adapter names it);
    `members` as above, the label being the native id. No contest directory
    and no membership are written.

  A CSES contest is not a selection: the adapter refuses it as `unsupported`
  (U9).
- **Bulk bound** (U9, storage side): the selected members are the whole
  enumeration, or the members named by `--only <labels>` (a label not in the
  enumeration → refused, `unknown_label`). More than **50** selected members
  → refused, `selection_too_large{count}`. Both refusals happen before the
  first member transaction: no download area, no fetch record and no contest
  file is created. That the enumeration costs exactly one request and each
  member one more, and the per-invocation budget (200 requests, 15 min), are
  sites-contract properties verified by the adapter tests (C4, C8); the
  parsed-result harness of §10 cannot observe requests.

#### 5.8.2 Records

`<p>` is the problem directory name (`<site>-<native>`, §2.1).

| Record | Class | Written | Meaning |
|---|---|---|---|
| `problems/<p>/tests.toml`: entries with `download`, table `[fetch]` | shared; tool fields (§8.1) | step 5 (creation: step 4) | **commit record**: test membership; `[fetch] txn` names the last committed fetch |
| `problems/<p>/problem.toml` `[problem]` `url`, `title`, `kind`, `limits` (`id` fixed at creation) | shared; tool fields | step 6 (creation: step 4) | last successful metadata |
| `contests/<c>/contest.toml` | shared | step 8 | contest membership |
| `.cpforge/fetch/<p>/journal.toml` | tool-only; no-clobber at step 3, atomic replace at step 6, removed at step 7 | steps 3, 6 | intent of the open transaction, complete for recovery |
| `.cpforge/fetch/<p>/txn-<txn>.toml` | tool-only, no-clobber, **never rewritten** | step 7, or an abort (§5.8.8) | durable history of one transaction |
| `.cpforge/fetch/<p>/status.toml` | tool-only, atomic replace | end of each attempt | latest attempt, kept apart from the metadata |
| `.cpforge/downloads/<txn>/` | tool, leased | steps 0–1 | download area |

- **Transaction id** `<txn>`: the job-id grammar (§6.2), generated at step 0;
  the exclusive `create_dir` of the download area makes it unique (an
  existing name → new random part). Txn ids are listed in string order;
  nothing depends on that order matching time (clock changes are harmless).
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
- **Journal** (`journal.toml`, schema 1). It holds everything reconciliation
  and history need, so recovery never reads the network, the download area,
  staging or an earlier catalog:
  - `txn`, `identity`, `create` (bool), `phase` (`"files"` from step 3,
    `"metadata"` from step 6);
  - `[base]`: `problem` and `catalog` (`"absent"` or the sha256 of the
    bytes), and one `[[base.entry]]` (`id`, `input`, `answer`: sha256 or
    `"missing"`) per download entry (§5.8.4 bases);
  - `[[planned]]`: `path`, `sha256`, `side` (`input` | `answer`),
    `ordinal`, `role` (`registered` | `unregistered-upstream`), in
    publication order;
  - `[catalog]`: the new `upstream_samples` and `max_ordinal`, and
    `[[catalog.op]]` in application order, each naming an entry by its
    stable `id`: `add` (`input`, `answer`, `download`), `replace` (new
    `input`, `answer`, `download`, and `expect_input`, `expect_answer`,
    `expect_download`: the entry's values at the base), `disable`
    (`expect_download`);
  - `[[metadata]]`: `field`, `old` (omitted when the field was absent),
    `new`;
  - `[metadata_step]` (phase `"metadata"` only): `after_sha256` (§5.8.4
    step 6);
  - `[history]`: the body of the committed history record (§5.8.10) without
    `schema` and `outcome`: `fetched_at`, `url`, every `[[pair]]` and every
    `[[file]]`, including each `superseded` file with the hash of the
    `download` table it replaced.

  The history record of the txn is derived from `[history]` alone:
  committed → `[history]` plus `outcome = "committed"`; rolled back or
  aborted → `fetched_at`, `url`, every `[[pair]]`, the outcome, and as
  `[[file]]` the files this txn published (live abort) or the present set
  (§5.8.7), each with role `unreferenced`. Given the same journal, a
  reconciled record is byte-identical to the one the live transaction would
  have written.

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
  own probe of its own held area returns `held`. No step of reclamation
  syncs a directory.
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
   it does not exist and the §2.1 resolution of `<p>` is not "create" (a
   local problem whose directory name has the remote form is refused here;
   `cpf add` does not reserve remote forms: settled); the catalog is not
   `missing` or `valid` (§8.1); a journal of `<p>` remains (classification
   other than `valid`, or reconciliation left it). Otherwise create the
   download area, record the bases, release the lock.
1. **Network phase**, without the workspace lock: the adapter produces the
   parsed problem, whose samples are written into the area.
2. **Revalidate** (lock, recovery; **held continuously through step 7**). A
   journal of `<p>` present now is reconciled first. Then every base is
   recomputed; any difference is the conflict `base_changed`: nothing is
   published, the status is recorded. **Editedness is observed here** (it
   equals the step-0 observation). The plan is computed from the catalog
   whose hash is the catalog base: pair outcomes (§5.8.5), file names
   (§5.8.6), catalog operations, metadata diff (one `(field, old, new)` per
   available field whose value differs). A plan with no file, no catalog
   operation, unchanged `[fetch] upstream_samples` and `max_ordinal` and no
   metadata diff is `unchanged`: no txn record, no journal, status only.
   **Size preflight**: the plan is refused (`record_too_large`, before step
   3) when the journal, the history record, the catalog after the step-5
   edit, or `problem.toml` after the step-6 edit rendered against its
   current bytes (the output bytes of a §4.4 prepared edit, then dropped)
   would exceed 1 MiB (§9).
3. **Journal**: publish `.cpforge/fetch/<p>/journal.toml` no-clobber (§4.3,
   synced) with phase `"files"` and every field of §5.8.2. A refetch
   publishes from the area directly; only creation stages.
4. **Publish files** no-clobber (§4.3), in plan order (input before answer
   of each pair), each streamed from the area. The SHA-256 of the bytes
   written to the temporary file must equal the planned hash before the
   publishing call. Immediately before each publishing call (barrier
   `fetch.before_publish#i`, after the temporary file is written) the name is
   rechecked by collision key (§2.1) against a fresh listing of `tests/`.
   A hash mismatch stops publication (`failed`); `Exists`, or a key-equal
   entry found by the recheck, stops it (`name_taken`, also on filesystems
   that would accept a case variant); either aborts (§5.8.8). Each
   publication syncs the file before its rename and, on Unix, the directory
   after it (§4.3).
5. **Commit the catalog** (not for creation). If the catalog base is
   `absent`: in a live transaction the catalog is first created no-clobber as
   exactly `schema = 1\n` (as §5.3; `Exists` → `Conflict`); in a
   reconciliation roll-forward a current file of exactly those bytes is
   used as it is, without a second creation, and only a missing catalog is
   created (`Exists` → `Conflict`). Either way the expected baseline is then
   the hash of `schema = 1\n`; otherwise it is the catalog base. Then one §4.4 edit of `tests.toml`, without the replay
   wrapper, with two additions:
   - **Expected baseline**: the SHA-256 of the bytes read at §4.4 step 1 must
     equal the expected baseline, else `Conflict` (`base_changed`) before any
     operation is applied. The operations are applied by stable id; each
     `replace` and `disable` requires the entry to exist with exactly its
     `expect_*` values and each `add` requires that no entry has that id or a
     key-equal path (implied by the baseline, checked anyway: a failed
     precondition is `Conflict`). The edit sets `[fetch] txn`,
     `upstream_samples` and `max_ordinal`. (This is an expected-hash
     parameter of §4.4 step 1, added for fetch.)
   - **Rehash**: each publish attempt (§4.4 step 4, every Windows retry
     included) also rehashes (a) the input and answer of every entry a
     `replace` changes, against their bases, and (b) every planned file the
     edit registers, against its planned hash; a difference is `Conflict`
     (`edited_during_commit`).

   `Conflict` or any other error leaves the catalog uncommitted (§4.1) and
   aborts (§5.8.8). As §4.4 states, an editor save between the last
   validation and the rename is not detected (the remaining race, §5.8.11).
6. **Metadata** (refetch with a metadata diff only). `problem.toml` not
   `valid`, or no longer bound to the identity → not edited, finding
   `metadata_not_applied{invalid | unbound}`, step 7. Otherwise `prepare`
   the §4.4 edit (prepared form) applying the per-field rule to each diff
   field: current = old → set new; current = new → nothing; otherwise kept,
   finding `metadata_conflict{field}`. *Unchanged* → step 7. Output bytes
   above 1 MiB → `metadata_not_applied{too_large}`, step 7. Otherwise
   **replace the journal** (§4.2) with phase `"metadata"` and
   `after_sha256` = SHA-256 of the prepared output bytes (crash point
   `fetch.metadata_prepared`), then `publish` the prepared edit. A
   `Conflict` repeats step 6 once (new `prepare`, new journal replacement).
   Every other result maps as follows: a permanent refusal → not edited,
   `metadata_not_applied{reason}`, step 7: `NotSingleRegularFile`
   (`not_single_file`: a link, a non-regular file, more than one hard link),
   `TooLarge`, `Encoding`, `Parse` or `Check` (`invalid`), `Operations`
   (`unbound`), `LinkInPath`, `InvalidPath`, `PathTooLong`, `Unsupported`,
   `CrossFilesystem`; a retryable failure → §5.8.8 `metadata_pending`: a
   second `Conflict`, `Io` (including permission denied and the Windows
   sharing timeout), a failed journal replacement.
7. **Finish**: publish `txn-<txn>.toml` no-clobber from the journal's
   `[history]`; replace `status.toml`; remove the journal (unlink; Windows
   retries a sharing violation with the §4.2 step 3 backoff; then, on Unix,
   sync `.cpforge/fetch/<p>/`); release the area (§5.8.3). Failures:
   §5.8.8.

**Creation** (no `problems/<p>` at steps 0 and 2): step 3 writes the journal
with `create = true`; step 4 stages `.cpforge/staging/<txn>/` with the same
problem files as §5.2 step 2 (identity, template solution under the same
rules, `[files] created`, `created_at`), the tool fields of `problem.toml`, `tests.toml`
(`schema = 1`, `[fetch]`, one entry per pair) and the sample files under
the base names, then `publish_dir_noclobber` to `problems/<p>`. That
publication commits steps 4–6 at once; a non-committed publication is
reclassified by §2.1 and aborts with nothing published. Step 7 follows.
Zero samples give an empty catalog (`upstream_samples = 0`, `max_ordinal =
0`, no entries) and the warning `no_samples`.

**Problem sets of several problems** run one transaction per problem, in
selection order, each with its own txn, area and lock acquisitions; a
`Contest` selection then runs step 8.

#### 5.8.5 Pair rules

- A **sample** is an (input, answer) pair. A download entry is **edited**
  when either file's state at the bases differs from its `download` hash
  (bytes differ, or `missing`). Its **recorded pair** is
  `(input_sha256, answer_sha256)`; for an upstream pair `U_k`, the hashes of
  its canonical bytes.
- `n` = number of upstream pairs. `m` = `[fetch] max_ordinal` (0 when
  `[fetch]` is absent), raised to the largest ordinal of any download entry
  (a hand-lowered value cannot re-add a registered ordinal). `max_ordinal`
  is the highest upstream ordinal any committed fetch has seen: each commit
  sets it to `max(m, n)`, so it never decreases, whatever the upstream count
  does later. `upstream_samples` is only the latest count (`n`), for
  display and the `unchanged` test.
- **Identical upstream**: every download entry's ordinal `k` has
  `U_k` = its recorded pair, and no `U_k` lacks an entry → no pair
  outcome; the checks below are skipped.
- **Zero samples**: `n = 0` with at least one download entry → the test set
  is kept: no pair outcome, no catalog operation, `upstream_samples` and
  `max_ordinal` unchanged, `--disable-removed` ignored, warning
  `zero_samples`.
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
| no entry, `k > m` | present | `added`: publish under the base name if free, else a revision name (§5.8.6); append entry `id` = stem, `origin = "sample"`, `enabled = true`, `download`; files role `registered` |
| no entry, `k ≤ m` | present | `removed_locally`: nothing published or added (also after any upstream shrink and regrowth) |
| no entry | absent | nothing |
| entry, unedited | = recorded | `unchanged` |
| entry, unedited | ≠ recorded | `updated`: publish a revision pair; `replace` the entry's `input`, `answer`, `download` (id, `enabled` and unknown keys kept, so a disabled entry stays disabled); new files `registered`, old files `superseded` |
| entry, edited (input, answer or both; a file missing) | = recorded | `kept_edited`: nothing |
| entry, edited | ≠ recorded | `kept_edited`: the whole pair and its entry are kept; the upstream pair is published as an unregistered revision pair (role `unregistered-upstream`) unless already retained (below); the warning `edited_sample_kept{test, upstream}` names the entry and the upstream pair's stem, on stderr and in the `fetch` document; exit status unaffected (0 by itself) |
| entry (any) | absent (`k > n ≥ 1`) | `removed_upstream`: reported; with `--disable-removed` the edit sets `enabled = false` (`disable`); files untouched |

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
  catalog id. Names are chosen under the step-2 lock and rechecked before
  each publication (§5.8.4 step 4).
- `added` uses the base stem if free, else the smallest free revision stem;
  `updated` and `unregistered-upstream` always use the smallest free
  revision stem. An existing unregistered `sample-1.r2.in` (or any file of
  that name) makes the choice skip to `r3`. A name is never reused for other
  content, so a name and hash pair identifies one publication.

#### 5.8.7 Reconciliation of a journal

Runs under the lock: in `cpf fetch` at steps 0 and 2 for that problem, and in
`cpf fsck --apply` for every journal. `cpf fsck` and readers only report.
Other commands do not reconcile. It reads the journal and the current
workspace only.

- Classification (§9 record): `valid` → below. `too_new`, `unreadable`,
  `malformed` → kept, reported, and `cpf fetch` of `<p>` refused. `fsck
  --apply` removes a `malformed` journal (reported); the files it named are
  then user content. `too_new` and `unreadable` journals are never removed.
- **Commit marker.** Refetch: `tests.toml` `valid` and `[fetch] txn` = the
  journal's txn → *committed*; `tests.toml` `missing`, or `valid` with
  another or no `[fetch] txn` → *not committed*; `unreadable`, `malformed`
  or `too_new` → *undeterminable*. Creation: `problems/<p>` absent, not a
  directory, or a directory whose valid `problem.toml` is bound to another
  identity → *not committed*; a directory bound to the journal's identity →
  the refetch rule on its `tests.toml`; a directory whose `problem.toml` is
  not `valid` → *undeterminable*.
- **Decision**, first matching rule; `H` is `txn-<txn>.toml` of `<p>`:
  1. `H` `valid` with this txn and identity and outcome `committed` →
     **finished**: no catalog or metadata edit (a `problem.toml` changed
     after the crash, even back to an old value, is kept); status
     `committed`, `recovered = true`; journal removed.
  2. `H` `valid` with this txn and identity and another outcome →
     **decided, not committed**: if the marker is *committed* →
     `fetch_inconsistent` (below); otherwise status `rolled_back`, journal
     removed, no second record.
  3. `H` present otherwise (`malformed`, `unreadable`, `too_new`, another
     txn or identity) → `history_blocked{path, classification}`: journal
     kept, nothing changed, `cpf fetch` of `<p>` refused. cpf never removes
     it (§5.8.10); once the user moves it away, the next reconciliation
     decides again.
  4. Marker *undeterminable* → journal kept, nothing changed, reported as
     `fetch_incomplete` `undetermined`; decided again once the record is
     repaired.
  5. Marker *committed* (creation: step 6 is part of the directory) →
     metadata by phase, refetch only: `"files"` → step 6 (§5.8.4, per-field
     rule; step 6 never started, so no publication can be repeated);
     `"metadata"` → never edited by reconciliation: current `problem.toml`
     SHA-256 = `after_sha256` → step 6 done; anything else (not yet
     published, or published and since changed or exactly restored: the
     hashes cannot tell these apart) → `metadata_not_applied{interrupted}`,
     and the next refetch recomputes the diff from the current values. Then
     step 7; status `committed`, `recovered = true`.
  6. *Not committed*, refetch. *Present set* = planned files present as
     regular files with their planned hash. **Roll forward** (steps 5–7 with
     the step-5 expected baseline and rehash, outcome `committed`,
     `recovered = true`) only if the present set is every planned file and
     every base still matches (a base `absent` catalog also matches a file of
     exactly `schema = 1\n`, which step 5 then reuses, §5.8.4). Otherwise,
     or when the roll-forward's step 5 does not commit, **roll back**: history `rolled_back` derived from the
     journal (§5.8.2; written only when the present set is not empty),
     status `rolled_back`, journal removed. No file is ever deleted.
  7. *Not committed*, creation → never rolled forward (its staging is
     removed by recovery); status `rolled_back`, journal removed.
- `fetch_inconsistent` (a committed marker with a non-`committed` record, or
  identity mismatches between journal and record) arises only from hand
  edits of tool fields or records: journal kept, nothing changed, fetch of
  `<p>` refused, reported by `fsck`.
- A planned name present with another hash, or as another file type, is
  user content: not recorded, never deleted (§5.8.10).
- Failures of the steps reconciliation runs follow §5.8.8.

#### 5.8.8 Abort and failure transitions

An abort after step 3 (publication failure, `name_taken`, a non-committed
step 5) is the roll back of §5.8.7 performed by the owner under its lock:
history with outcome `conflict` or `failed` derived from the journal
(§5.8.2), listing the files this txn published as `unreferenced` (written
only when it published any), status, journal removal, area release. The
published files stay as unregistered downloads. Before step 3 an abort
writes only the status. An abort whose record cannot be written (other than
`Exists`) persists no decision: the journal stays **undecided**, and the next
reconciliation decides by rule 6, which may commit it. That is sound because
nothing was committed and rule 6 rechecks everything: a conflict abort fails
the base check, a `name_taken` or hash-mismatch abort lacks a planned file,
and an abort after a transient step-5 error may legitimately roll forward.

Every failure, live or during reconciliation, has one transition. "Kept"
journals are decided again by the next reconciliation (§5.8.7); retries of
publication primitives (Windows ≤ 2 s, §4.2–§4.3) happen inside the step.

| Step | Failure | Committed | Journal | History | Status outcome, finding |
|---|---|---|---|---|---|
| 3 | journal publication error | no | none | none | `failed` |
| 4 | publish error, hash mismatch, `Exists`, key recheck | no | removed (abort) | `failed`, files published so far | `failed`; `name_taken` for the last two |
| 5 | `Conflict` (baseline, precondition, rehash, validation), schema-only creation `Exists` | no | removed (abort) | `conflict` | `conflict` (`base_changed` / `edited_during_commit`) |
| 5 | any other error (I/O, sharing timeout, `TooLarge`) | no | removed (abort) | `failed` | `failed` |
| 3–7 | a committed publication with `Durability::Uncertain` or `Cleanup::Pending` | yes (that step) | continues | continues | warning |
| 6 | not valid, unbound, too large, or a permanent edit refusal (the §5.8.4 step 6 list, e.g. a second hard link) | yes | continues to 7 | written | `committed`, `metadata_not_applied{reason}` |
| 6 | second `Conflict`, `Io` (permission, sharing timeout), failed journal replacement | yes | kept | none | `committed`, `metadata_pending` (next: rule 5) |
| 7 | history publication error | yes | kept | none | `committed`, `history_pending` |
| 7 | history `Exists` (§5.8.7 rule 3) | yes | kept | blocked | `committed`, `history_blocked` |
| 7 | status replacement error | yes | continues | written | warning of the command only |
| 7 | journal unlink error after retries | yes | kept | written | `committed`, `journal_pending` (next reconciliation: rule 1) |
| abort | history publication error other than `Exists` | no | kept, **undecided** (no abort decision is persisted) | none | `conflict`/`failed`, `abort_unrecorded` (next: rule 6 decides afresh: roll forward if every planned file is present and every base matches, else roll back with `rolled_back`) |
| abort | history `Exists` | no | kept | blocked | `conflict`/`failed`, `history_blocked` (next: rule 3) |
| abort | journal unlink error | no | kept | written | `conflict`/`failed`, `journal_pending` (next: rule 2) |
| reconciliation | undeterminable marker, blocked history, inconsistency | — | kept | unchanged | not replaced; reported by `fetch` (refusal) and `fsck` |

The area is released after every row except where the owner died. A
`*_pending` journal blocks only later fetches of `<p>` until it is
reconciled; readers report it (§5.8.12).

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
  with the hash of the replaced `download`). Derived from the journal
  (§5.8.2), published no-clobber, never rewritten or removed by cpf
  except by archive retirement, which moves
  `.cpforge/fetch/<p>/` with its problem after the records are in a
  committed snapshot (archive contract §5.3).
- **Size**: each record is bounded (64 pairs, at most four files per pair,
  cpf-chosen names, catalog paths ≤ 255 bytes) and checked at step 2. The
  *set* of records is not bounded: it grows by at most one record per
  transaction that wrote a journal (an `unchanged` fetch writes none), and
  provenance reads every record of the problem.
- **Tool-published**: a file under `tests/` counts as published by cpf only
  when its name and current SHA-256 appear in the open journal (a `valid`
  journal, as one pseudo-record of its txn with the planned roles) or in a
  `valid` history record of `<p>` whose `identity` is the problem's
  identity. Anything else is user content. Records of another identity
  (a directory name reused) or not `valid` contribute nothing.
- **Provenance** (workspace design §2.2, amended): references in
  `tests.toml` / `problem.toml` first (`registered`); then
  `downloaded{role, txn}`; then `[files] created`, `[files] user`, otherwise
  `unknown`. For `downloaded`, collect every matching `[[file]]` (same path
  and current SHA-256) of the records above; the role with the highest
  precedence wins: `superseded` > `registered` > `unregistered-upstream` >
  `unreferenced`; `txn` is the greatest txn id (string order) among the
  matches with that role. So a file replaced by an update is `superseded`
  whatever the order of the two records, and the result does not depend on
  clocks, on listing order or on whether the journal is still present. A
  registered download entry whose file differs from its `download` hash is
  reported as edited.
- Displays: `cpf status` shows counts by role and the latest txn; `cpf fetch
  --history` lists every record (a non-`valid` one by classification).
  Records move with their problem into an archive.
  They are class `fetch` of the snapshot, always kept, and are restored
  with the problem unless `--into` changes its identity (archive contract
  §7.3, §7.4).

#### 5.8.11 Observation points and the remaining race

- Editedness and every base are observed at step 0 and revalidated at
  step 2 (lock held from then on); the catalog bytes read by the step-5
  edit must equal the catalog base; samples are rehashed again in every
  step-5 publish attempt; the catalog itself is validated by §4.4 step 4.
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
are unregistered candidates), `committed` (tests committed, metadata or
history pending: old metadata with new tests), `finished` (history written,
journal removal pending) or `undetermined` (§5.8.7 rules 3–4 and
`fetch_inconsistent`, with the reason). The phase is derived with the
§5.8.7 decision rules, read-only. Jobs read the committed
catalog; nothing they select depends on a journal.

#### 5.8.13 Contest membership (step 8)

- A `Contest` selection (§5.8.1) resolves the parsed contest first, applies
  the bulk bound, then runs one transaction per selected member, then one
  membership edit under the lock.
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
- A `ProblemSet` selection writes no membership and no contest directory.

### 5.9 Organisation: tags, workflow status, notes, listing, search

Tags, the user's workflow status, dates and notes of design §7a, and the
read-only `cpf ls` and `cpf search` over them. These commands start no
process, take no job lease and run no hooks, so they run in full on Linux,
macOS and Windows; every test of this section runs on all three.

**Two statuses, two names.** The **workflow status** is the value the user
sets with `cpf mark` (`[problem] status`: `new`, `solved`, …). The
**workspace status** is what `cpf status` shows (registered files missing,
untracked files, run-fact comparison; workspace design §3.3). `cpf mark`
never reads facts and `cpf status` never derives a workflow status: passing
tests never sets `solved`. `cpf status` shows the workflow status as one
labelled line `workflow: <value>` next to the tags, and its JSON member
stays `user_status` (report §6.1). Contests have no stored workflow status;
`cpf ls --contests` shows a **derived** one (§5.9.7).

#### 5.9.1 Fields and where they live

| Field | Record | Type | Written by | Never written by |
|---|---|---|---|---|
| `tags` | `problem.toml` `[problem]` | array of tags (§5.9.2) | `cpf tag`, `cpf untag` | any other command |
| `status` | `problem.toml` `[problem]` | a status name (§5.9.2) | `cpf mark` | any other command; jobs and facts never |
| `created_at` | `problem.toml` `[problem]` | timestamp | `cpf add` (staged record, §5.2 step 2); fetch creation (§5.8.4, staged record) | every later command; never backfilled |
| `updated_at` | `problem.toml` `[problem]` | timestamp | every committed `tag`, `untag`, `mark` edit of that record | fetch metadata edits (§5.8.4 step 6), `adopt`, test commands, `notes` |
| `solved_at` | `problem.toml` `[problem]` | timestamp | `cpf mark <t> solved` when the stored status is not `solved` and `solved_at` is absent | everything else; no command changes or removes it |
| `archived_at` | `problem.toml` `[problem]` | timestamp | the archive (archive contract) | every §5.9 command (read only) |
| `tags` | `contest.toml`, top level | array of tags | `cpf tag`, `cpf untag` on a contest | any other command |
| — | `problems/<p>/notes.md` | free text | created empty, once, by `cpf notes` (§5.9.5) | everything: never modified, truncated or deleted |

- **In the records, not a sidecar.** The fields are user fields of
  `problem.toml` and `contest.toml` (§3), written only through the §4.4 edit
  transaction. Reasons: design §7a stores them there so they travel into an
  archive with the problem; the user edits them by hand next to the rest of
  the record; `cpf status` already reads them there; the edit transaction
  already gives the crash and editor semantics wanted, so no new record,
  schema or recovery rule is needed. A sidecar would add a second file per
  problem whose consistency with `problem.toml` cpf would have to define.
- Contest tags sit at the top level of `contest.toml` (beside `schema`),
  not in `[contest]`: that table holds fetch's tool fields and its presence
  binds a remote contest (§5.8.13), while a local contest has none. Contests
  have no dates and no workflow status.
- **Field states.** Each field above is `absent`, `valid` or `invalid{reason}`
  by the rules of §5.9.2. The state of an organisation field is **not** part
  of record classification (§9): a `problem.toml` whose `tags` is invalid is
  still `valid`, selectable and buildable, so a typo never blocks a job.
  Every reader reports an invalid field (§5.9.9); a filter on an invalid
  field never matches; a command that would write an invalid field refuses
  (§5.9.4).
- `archived_at` on an active problem is shown and filterable but has no
  meaning for this section; the archive contract defines it.
  The archive sets it only in the snapshot's copy of
  `problem.toml`; an active problem has it after a restore.
- `notes.md` is a fixed name of the problem layout, like `problem.toml` and
  `tests.toml`: its provenance is never `unknown` and `cpf status` never
  lists it as untracked (workspace design §2.2, amended).

#### 5.9.2 Grammars

- **Tag**: 1–32 ASCII characters from `[A-Za-z0-9_-]`, the first a letter
  or digit. The **tag key** is its ASCII lowercase form (equal to the §2.1
  collision key for these characters). Tags are stored as first spelled;
  two tags with the same key are the same tag. Tags are free-form: there is
  no configured tag set.
- **Valid `tags` field**: an array of at most 64 strings, each a tag, no two
  with the same key. Anything else (not an array, a non-string or
  ungrammatical element, a duplicate key, more than 64) is
  `invalid{reason}`, the reason naming the first offending element.
- **Status name**: 1–32 ASCII characters from `[a-z0-9-]`, the first a
  letter, the last not `-`. A stored `status` that is a string in this
  grammar is `valid`, whether or not the current vocabulary lists it (a
  removed custom value stays readable and filterable); any other value is
  `invalid`.
- **Vocabulary** (used only by `cpf mark`): the built-in names `new`,
  `in-progress`, `solved`, `skipped`, in this order, followed by
  `status.values` (configuration contract §7.5) in configured order.
- **Effective status**: the stored `status` when `valid`; `new` when
  `absent`; none when `invalid`. Filters and contest counts use the effective
  status, so a problem never marked counts as `new`.
- **Timestamp** (all date fields): a TOML **string**. cpf writes exactly the
  report `time` form `YYYY-MM-DDTHH:MM:SS.mmmZ` (report §3.2): UTC from the
  system clock, milliseconds truncated, so values sort as strings and
  appear unchanged in JSON. A reader also accepts the hand-written forms
  `YYYY-MM-DDTHH:MM:SSZ` (an instant) and `YYYY-MM-DD` (a **date**, not an
  instant). The parsed value keeps that distinction: an *instant* or a
  *date*; only the report encoder writes a date as `T00:00:00.000Z`
  (report §6.1), and nothing else converts it. Every
  component is range-checked (month 01–12, day valid for the month and leap
  year, hour 00–23, minute and second 00–59; no leap second). **Domains:**
  an instant lies in `1970-01-01T00:00:00.000Z` ..
  `9999-12-30T22:00:00.999Z` inclusive (the upper end is jiff's
  `Timestamp::MAX` truncated to milliseconds, so every accepted instant
  converts to a local date in every zone); a date lies in `1970-01-01` ..
  `9999-12-31`. A value outside its domain, a TOML date-time value, a local
  time, another offset or any other string is `invalid`. A system clock
  outside the instant domain refuses the writing command (8) before
  anything is written. Times are informative: a
  clock step can make `updated_at` earlier than `created_at`; nothing
  depends on their order.

#### 5.9.3 Targets

`cpf tag`, `untag` and `mark` take an explicit first operand `<target>`,
because a local problem name and a tag are both bare words:

| `<target>` | Means |
|---|---|
| `.` | the problem this invocation selects (§5.7 sources 2–3: cwd problem, then saved selection) |
| a problem reference | §5.7 references, extended by the remote (§2.1) and `<contest>/<label>` forms; resolved as a problem only, never as a contest |
| `contest:<name>` | the contest `contests/<name>/` (`<name>` an Identifier); `tag`, `untag` only |

A bare name that names no problem is the error "no problem `<name>`" (8),
with a note when a contest of that name exists: mutations never fall back
from a problem to a contest (`cpf status` does, as a reader). The global
`--problem` flag with any of the three commands is a usage error (2), as
with `switch`. `cpf notes` takes an optional problem `[<ref>]`; without it
the problem is selected as for every command (§5.7, `--problem` included);
both a `<ref>` and `--problem` → 2.

#### 5.9.4 `cpf tag`, `cpf untag`, `cpf mark`

```
cpf tag <target> <tag>...
cpf untag <target> <tag>...
cpf mark <target> <status>
```

**Before the workspace.** Arguments are checked first, before discovery,
config or the lock: at least one tag; every tag in the §5.9.2 grammar; for
`mark`, exactly one status in the status-name grammar and a target that is
not `contest:<name>`. A violation is a usage error (2) and nothing is
opened. (Such an error lies in the argument alone and is found before any
record or configuration is read, which is what 2 means, report §5.1; 7 is
kept for what depends on configuration.) `mark` then loads configuration
layers 0–2 (no problem layer). It refuses with 7 only when the effective
`status.values` cannot be determined: a diagnostic at `status.values`, or
a layer that is invalid as a whole (configuration contract §3.2). Other
diagnostics (a broken language, say) are printed as warnings and do not
block marking. A status not in the vocabulary → 7, naming the vocabulary.
`tag` and `untag` read no configuration.

**Transaction**, one per invocation:

1. Lock (leftover recovery first, §4.5). Resolve `<target>` under the lock
   (§5.7, or the contest directory: an Identifier, not a link, a directory,
   `contest.toml` present). Record the **expected binding**: for a problem
   its identity (bound to the directory, §2.1); for a contest the presence
   and value of `[contest] id`.
2. One §4.4 edit transaction on `problems/<p>/problem.toml` or
   `contests/<c>/contest.toml`, with the replay wrapper (at most two
   attempts, each from a fresh read). The schema check is the record's
   classification (§9; for a contest the §5.2 membership rules too):
   anything but `valid` → refused (8), nothing written. The check then
   compares the fresh document with the expected binding: a problem whose
   `[problem] id` is no longer the resolved identity, or a contest whose
   `[contest] id` appeared, disappeared or changed → refused
   (`IdentityMismatch`, 8), nothing written, on the first attempt and on the
   replay alike. The operation, on the fresh document of each attempt:
   - `tag`: if `tags` is `invalid` → refuse (8, naming the field and
     reason). The arguments are first reduced to one tag per key (the
     first spelling wins). Then append each of them, in argument order,
     whose key is not already present; a missing `tags` is created as an
     inline array in `[problem]` (problem) or at the top level (contest).
     The array's existing elements, their spelling, order and formatting
     are kept. If the result would hold more than 64 tags → refused (8,
     `too_many_tags`), nothing written; the count is computed on the fresh
     document of each attempt, so an editor's addition before the replay
     counts.
   - `untag`: if `tags` is `invalid` → refuse (8). Otherwise remove every
     element whose key equals an argument's key; the key stays, possibly as
     `tags = []`. A tag that is not present is reported on stderr and is
     not an error.
   - `mark <s>`: if `status` is `invalid`, or `<s>` is `solved` and
     `solved_at` is `invalid` → refuse (8). If the stored `status` already
     equals `<s>` (exact string) → no change, even when `solved_at` is
     absent (cpf does not invent the time of an earlier solve). Otherwise
     set `status = "<s>"`; when `<s>` is `solved` **and `solved_at` is
     absent**, set `solved_at` to now. A `valid` `solved_at` is never
     changed or removed: it records the **first** time cpf saw the problem
     marked solved (settled), so reopening and re-solving keeps it.
   - Every operation that changes the document of a **problem** also sets
     `updated_at` to now (refused, 8, if the stored `updated_at` is
     `invalid`). A contest edit writes no date.
   "Now" is read once per invocation, before step 1.
3. No change → nothing is written (§4.4 step 3), exit 0, a stderr note
   says the record already had that state. A result over the record cap
   (§9) → refused (8), nothing written. A second `Conflict` → 8, the
   record as the editor left it.

Only these keys change; every other byte of the record round-trips (§4.4
step 2). Output: nothing on stdout; one stderr line naming the target and
the resulting tags or status. No `--json`, no `--events` (2). Statuses:
0, 2, 7 (`mark` only), 8. Windows sharing violations follow §4.2's ≤ 2 s
retry with revalidation, then 8.

#### 5.9.5 `cpf notes`

```
cpf notes [<ref>] [--path] [--json]
```

- **Mutating form** (default). Resolve the problem (§5.9.3), then lock
  (leftover recovery first) and inspect `problems/<p>/notes.md` without
  following links:
  - absent → `publish_file_noclobber` (§4.3) of an **empty** file; state
    `created`;
  - a regular file (any content, any size) → untouched, not opened for
    writing; state `existing`;
  - `Exists` from the no-clobber publish (an editor or another `cpf notes`
    created it after the inspection) → inspect again: a regular file is
    `existing`; anything else as below;
  - a link, directory or other non-regular entry → refused (8), nothing
    written, stdout empty.
  Then print the absolute path of `notes.md` and a newline on stdout (the
  `cpf current --path` form, §5.7), and on stderr `created` or `exists`.
  cpf never writes, truncates, renames or deletes an existing `notes.md`,
  and never opens an editor (`$EDITOR "$(cpf notes)"` is the intended use).
- **`--path`**: read-only. No lock, no recovery, nothing created: resolve
  the problem lock-free (§5.7 readers), print the same path, and say on
  stderr whether the file exists (`absent`, `existing`, or `not a regular
  file`). Exit 0 in each of these cases.
- `--json`: the `notes` document (report §6.1) instead of the bare path;
  on failure the `error` document (`command: "notes"`).
- A contest target (`contest:<name>`) → 2. Statuses: 0, 2, 8, 11
  (`--json` delivery).

#### 5.9.6 Filters

One grammar, parsed by one function, shared by `cpf ls`, `cpf status` and
`cpf search`:

| Flag | Value | Matches a problem when |
|---|---|---|
| `--status <s>` (repeatable) | status name | its effective status equals one of the values (OR) |
| `--tag <t>` (repeatable) | tag | its valid `tags` contains every value by key (AND); contest tags never count |
| `--site <s>` (repeatable) | `local` or the site grammar (§2.1) | its identity's site equals one of the values (OR) |
| `--contest <c>` (repeatable) | Identifier | a `[[problem]]` record of `contests/<c>/contest.toml` has its identity (OR over values) |
| `--created <range>`, `--updated <range>`, `--solved <range>` | range | that field is `valid` and its **local date** (an instant converted as below; a date value as written) lies in the range |
| `--archived-in <range>` | range | as the date filters above, for `archived_at` (active problems and, with `--archived` or in `search`, snapshots) |

- **Range**: `D` (that day), `D..` (from D), `..D` (up to and including D),
  `D1..D2` (inclusive); `D` is `YYYY-MM-DD` with the §5.9.2 component
  checks. `D1` after `D2`, `..`, or an empty side on both → 2.
- **Local days.** Ranges are in **local** days of the time zone in effect
  when the command starts; stored timestamps stay UTC (§5.9.2). A stored
  UTC instant has the **local date** obtained by converting it with that
  zone's offset at that instant (the zone's full rules, historical changes
  included). A value matches `D1..D2` iff its local date lies in
  `[D1, D2]`; open sides are unbounded. Because each instant has exactly one
  local date, DST needs no special case: a day whose midnight is skipped
  (a spring-forward gap at 00:00) begins at the first existing local time,
  a day with a repeated hour simply contains more instants, and a repeated
  midnight (fall-back at 00:00) belongs to the new day from the first
  instant whose local date is the new date. A hand-written date-only value
  (`YYYY-MM-DD`) is already a local date and is compared as written, never
  converted; the JSON documents still show it as `T00:00:00.000Z` (report
  §6.1).
- **Time zone backend**: the `jiff` crate, pinned to an exact `0.2.x`
  version whose `rust-version` is ≤ 1.85, with `default-features = false`
  and only `std`, `tz-system` (system zone discovery: `TZ`, then
  `/etc/localtime` on Linux and macOS; the Windows system zone mapped to its
  IANA name), `tzdb-zoneinfo` (the system TZif database on Linux and macOS)
  and `tzdb-bundle-platform` (a bundled database only where the system has
  none, i.e. Windows). The exact version, its declared MSRV and the feature
  set are verified by the first organisation task before the dependency is
  added (dependency review as for the HTTP stack, including an MSRV build on
  every release target). cpf writes no time zone parser of its own.
- **Time zone source**, read once per invocation, before any filter is
  evaluated, in this order:
  1. `TZ` set and non-empty (every OS) → jiff's interpretation of it
     (zone name, `:`-path, absolute path or POSIX string).
  2. Linux, macOS → jiff's system lookup (`TimeZone::try_system`:
     `/etc/localtime`), unmodified.
  3. Windows → cpf reads `GetDynamicTimeZoneInformation` first. If
     `DynamicDaylightTimeDisabled` is TRUE **and** `DaylightDate.wMonth` is
     0 (the "adjust for daylight saving time automatically" setting is off:
     no transitions at all), the zone is `TimeZone::fixed` with offset
     −`Bias` minutes (`StandardBias` is ignored when `StandardDate` is
     absent). Otherwise, including
     `DynamicDaylightTimeDisabled` TRUE with transition dates present (fixed
     yearly rules instead of dynamic ones), jiff's system lookup (the
     Windows zone mapped to its IANA name) is used; the year-specific
     differences of that fixed-rule case are not modelled. The decision is
     a pure function of the structure's fields.
  cpf's own rules are rule 3's fixed-offset case and the failure case: if
  the lookup fails (an invalid `TZ`, an unreadable `/etc/localtime`, an unmappable
  Windows zone), date filters use UTC and the command prints one stderr
  warning naming the cause; nothing else changes, and commands without a
  date filter never look the zone up. Conversions use the zone's complete
  rules (TZif transitions, then its POSIX footer for later instants).
- **Test seam**: the filter evaluator takes a `jiff::tz::TimeZone` as a
  parameter; production passes the discovered zone. Unit tests pass zones
  built through jiff's API from data in the repository only:
  `TimeZone::posix` strings, and `TimeZone::tzif` with **fixture TZif
  files** copied from one pinned IANA tzdata release (public domain, with
  a provenance note naming the release and file hashes). No test uses
  `TimeZone::get` or a system or bundled database, so results never depend
  on the host. End-to-end tests set `TZ` on Linux and macOS to a POSIX
  string or to the absolute path of a fixture TZif file. The rule-3
  decision function is unit-tested on every OS with constructed
  structures.
- Different flags combine with AND. A problem whose field is `invalid` or
  `absent` (dates) never matches a filter on that field and is still
  reported (§5.9.9).
- Filter values are checked by grammar only, before discovery, config or
  the lock; a violation → 2. No vocabulary check: a status outside the
  current vocabulary is a legitimate stored value.
- `--contest <c>` naming a contest that does not exist, or whose
  `contest.toml` is not `valid` → 8, naming it.
- **Candidates.** `cpf ls` and `cpf search`: every entry of `problems/`.
  `cpf status`: the selected set (a problem, a contest's members in
  `contest.toml` order, or `--all`); **filters without a selector imply
  `--all`**. Filters then keep the matching candidates, in candidate order.
  `cpf status` without a filter behaves exactly as before this amendment.
- **Same answer.** On one workspace, `cpf ls F` and `cpf status --all F`
  name the same problems in the same order (directory name, byte order) for
  every filter set F, when every entry of `problems/` is a valid problem.
  With an invalid entry, `ls` lists it under `invalid` and exits 0, while
  `status --all` keeps its existing behaviour (8, naming the entry; report
  §6.1 `error` document); both report it, neither hides it.

#### 5.9.7 `cpf ls`

```
cpf ls [filters] [--json]
cpf ls --contests [--tag <t>]... [--site <s>]... [--json]
cpf ls --archived [filters] [--root <dir>]... [--json]     (archive contract §8)
cpf ls --archived --contests [--root <dir>]... [--json]
```

Lock-free and read-only (§5.9.9). Default form: one entry per valid problem
that passes the filters, by directory name in byte order, with identity,
effective status, tags, title (when the fetch field is present), the four
dates and the contests it belongs to; then every entry of `problems/` that
is not a valid problem (§2.1 lookup rules: link, not a directory, record
`missing`/`unreadable`/`malformed`/`too_new`, `IdentityMismatch`, an
invalid or colliding name), each with its reason; then field findings.

`--contests`: one entry per entry of `contests/`, by name. For a valid
contest: its tags, its member count, and the **derived contest status**:
the number of members per effective status, listing the built-in names in
vocabulary order and then other stored values in byte order, omitting zero
counts; members whose problem cannot be resolved (missing directory, not a
valid problem, identity mismatch) are counted as `unresolved`, and members
with an invalid status as `invalid`. Nothing about a contest status is
stored, so it is always current. `--tag` matches contest tags (never member
tags) and `--site` the `[contest] id` site (`local` when there is no
`[contest]` table). The problem-only filters (`--status`, `--contest`,
dates) with `--contests` → 2. An invalid contest is listed with its
reason.

Statuses: 0 (also with invalid entries or findings), 2, 8 (outside a
workspace; `problems/` or `contests/` cannot be enumerated; `--contest`
names no valid contest), 11 (`--json`). Every entry here is an active
problem; results carry `origin: active` (report §6.1); `snapshot` results
come with `--archived`.

#### 5.9.8 `cpf search`

```
cpf search <term>... [filters] [--no-notes] [--no-archives] [--root <dir>]... [--json]
```

- **Terms**: 1–16 positional terms, each non-empty, valid UTF-8 (a
  non-UTF-8 argument → 2) and at most 256 bytes. A problem is a **hit** when
  every term occurs as a substring of at least one searched field (terms
  may match different fields). Matching compares the §2.1 collision keys of
  term and field text (`NFD(CaseFold(NFD(x)))`), so it ignores case and
  canonical-equivalence differences; as a stated consequence, a term `e`
  also matches `é`.
- **Searched fields** of each candidate that passes the filters: `identity`,
  `title`, `url` (fetch fields, when present), `tags` (each element),
  `status` (stored, when valid), and `notes` (the first 64 KiB of
  `notes.md`) unless `--no-notes`. Invalid fields are not searched and are
  reported.
- **Notes reading**: opened without following links (`O_NOFOLLOW`, Windows
  `FILE_FLAG_OPEN_REPARSE_POINT`) and non-blocking (`O_NONBLOCK`), then
  checked through the handle: anything but a regular file (link, FIFO,
  device, socket, directory) is never read, reported `notes_not_regular`.
  At most 65 536 bytes are read, plus one byte to detect more: then the hit
  or non-hit carries `notes_truncated`. A UTF-8 sequence cut by the limit is
  dropped; other invalid UTF-8 is decoded with U+FFFD replacements and
  reported `notes_not_utf8`, and the file is still searched. Line numbers
  count LF-terminated lines from 1 (a CR before LF is ignored).
- **Bounds**, per invocation, counted across every source of the search
  (the workspace, then every archive root, archive
  contract §8):
  - *entries* E ≤ 100 000: each directory entry returned by the
    enumeration of `problems/` and each file open attempted (`problem.toml`,
    `notes.md`, and `contests/<c>/contest.toml` for each `--contest`, read
    once before the candidates) counts one, hits or not;
  - *bytes* B ≤ 64 MiB (67 108 864): every byte read from those files.
  **One inventory.** `problems/` is enumerated once per invocation, and
  `contests/` at most once (only when a `--contest` value must be
  resolved). Candidate resolution applies the §5.7 reader lookup and the
  §2.1 rules (spelling, collision key, link and type checks, identity
  binding) **against that inventory**, never by re-enumerating a directory.
  **Charged operations** (a closed set): (a) each entry returned by an
  enumeration, which includes the entry's type query (the type the
  enumeration reports, or one no-follow `symlink_metadata` of that entry
  when the platform reports none); (b) each file open attempt
  (`problem.toml`, `notes.md`, `contests/<c>/contest.toml`), which includes
  every query on the opened handle (type, length, identity) and the
  no-follow check of the final component. Each charged operation counts one
  in E; reads count only in B. The search performs no filesystem operation
  outside this set: the directories `problems/<p>/` are known from the
  inventory and never queried again.
  `problems/` is enumerated completely first (counting entries), then the
  candidates are examined in directory-name byte order. Before reading a
  file, the length reported by its open handle (for `notes.md`, at most
  65 537) is checked against the remaining byte allowance, and a file that
  grows while it is read is read at most to that allowance, so B never
  exceeds its bound. Before an action that would make E exceed its bound,
  or a read that would make B exceed it, the search stops: the problem being examined is neither a hit
  nor reported as examined, and the result is **incomplete** with
  `entry_limit` or `byte_limit`. If E reaches its bound during the
  enumeration, the search stops there and examines no candidate.
- **Incomplete** means some content that the query covers was not examined.
  Reasons, each listed once in this order: `entry_limit`, `byte_limit`,
  `notes_truncated`, `unreadable_source` (a candidate that is not a valid
  problem, an unreadable or non-regular `notes.md`, an invalid field),
  `cancelled`. A complete search has no reason. `--no-notes` never yields
  `notes_truncated`.
- **Cancellation** (Ctrl-C, SIGTERM; Windows Ctrl-C, Ctrl-Break; report
  §5.3). `cpf search` installs the job commands' signal listener. All
  filesystem work runs on one worker thread, which checks cancellation
  before every counted action and publishes each finished candidate (hit or
  finding) to the main thread. The main thread waits for the worker **or**
  a cancellation notification. On cancellation it stops waiting at once,
  even while the worker is blocked inside a system call (a stalled
  enumeration, open or read on a slow filesystem), and writes the results
  published so far with `cancelled`. The candidate in progress is omitted.
  The blocked worker is abandoned and ends with the process; it holds no
  lock and writes nothing, so nothing is left to clean up. Guarantee: output
  starts within 200 ms of the first signal unless stdout itself blocks; a
  second signal at least 100 ms later forces exit (execution contract
  §8.2). The exit status follows report §5.2 (delivery failure first, then
  cancellation). The other commands of this section install no listener: a
  signal has its default effect (an interrupted edit is a crash state,
  §10). Slow or stalled archive roots get no timeout:
  this cancellation is the remedy, and `cpf ls --archived` installs the
  same listener and worker (archive contract §8).
- **Output**: hits in examination order: identity, `origin`, the matched
  field names, for notes up to 16 matching line numbers, and
  `notes_truncated`; then every finding; then, if incomplete, one line
  `search incomplete: <reasons>` on stderr. A search with zero hits says
  `no matches` and still lists findings and incompleteness: an unreadable
  source is never hidden behind "no matches".
- Lock-free and read-only (§5.9.9). No index or cache is written.
- Statuses: 0 (hits or none, complete or incomplete), 2, 8 (outside a
  workspace; `problems/` cannot be enumerated; `--contest` names no valid
  contest), 11 (`--json`), cancellation. Incompleteness is a reported
  result, not a failure, as findings are for `cpf status` and `cpf fsck`.
- **Archive roots**: after the workspace, every root of
  `archive.roots` and `--root` is traversed and each snapshot's summary is
  searched (identity, title, url, tags, status) together with the first
  64 KiB of its `problem/notes.md`, within the bounds above, with restore's
  containment rules (no link followed) and `origin: snapshot` results
  (archive contract §8). `--no-archives` searches the workspace only.

#### 5.9.9 Readers

`cpf ls`, `cpf search`, `cpf notes --path` and `cpf status` (with or
without filters) are **lock-free readers**: no lock, no leftover recovery,
no file created, renamed or deleted, no hook, no process; the workspace is
byte-identical afterwards (no `.cpforge/lock` created where there was none).
They read records with the §9 classification and resolve problems with the
§5.7 reader lookup. A concurrent **atomic replacement** (cpf's own edits,
editors that save by rename) can make an answer stale, never a mixture of
two states of one file. An editor writing **in place** gives no such
promise: a record or `notes.md` read during the write may contain old and
new bytes. Each file is read once into memory and that byte sequence is
classified and used as it is (a torn record may classify `malformed` and
is then reported, or may decode as a valid mixture); a reader never fails,
hangs or rereads because of it. Every source that cannot be used is
reported with its path and reason, in the human output and the document:
an entry of `problems/` that is not a valid problem, an invalid
organisation field (field name and reason), an unreadable `contest.toml`,
a non-regular or unreadable `notes.md` (search). A reader never stops at
the first such source (`cpf status` excepted, unchanged, §5.9.6).

#### 5.9.10 Maintenance

No tree is created. `cpf fsck` additionally reports invalid organisation
fields (problem and contest) and a non-regular `notes.md`; `--apply` never
changes them (user fields). `cpf clean` never touches `notes.md` or the
fields. `cpf gitignore` lists nothing new: notes and tags are user content.
The archive class `notes` (design §7a) is the archive contract's.

## 6. Job directories, leases, pruning

### 6.1 Layout

```
.cpforge/runs/<job-id>/            time-ordered + random id, never reused
    lease                          lock file held by the job owner
    job.toml                       schema, (epoch, n) admission sequence
    report.json
    inputs/<test-id>.in|.ans       frozen copies (§7)
    inputs/src/...                 captured solution-target sources (§7)
    inputs/capture.json            capture manifest (report contract §4.2)
    builds/<build-id>/             fresh output directory per build (build planning)
    <run-id>/  spec.json record.json cleanup.log  out/stdout out/stderr  cwd/
    <run-id>/out/output            extracted output file of a file-based I/O run (execution contract §5.6), outside cwd/
    <run-id>/judge/report, judge/tout  extracted judge artefacts (execution contract §2.1), outside cwd/
    <run-id>/transcript/           session transcript of a relay-mode interactor run
    inputs/roles/<role>/...        captured role files (stress: also generator, brute)
    iterations/<k>.<role>/         iteration run directory, the <run-id>/ layout (stress contract §3.1)
    failures/<id>/                 staged saved failure until promotion (stress contract §7.5)
    inputs/replay/input            replay: verified copy of a saved input (stress contract §8)
    inputs/bin/<role>/...          --replay-binaries: verified copies of preserved programs
    submit/out/                    the submit stage's {submit_dir}
    submit/version/                a version staged for publication
    submit/external.toml           external test directory record
.cpforge/runs/.reclaim-<job-id>/   being deleted
```

A stress job's `iterations/` holds, at any time, the
live iteration's directories, the persisted `spec.json`, `record.json`
and `cleanup.log` of its saved iterations, and at most one `keep`
iteration (stress contract §6.3); a passing or counted-only iteration's
directories are removed at its retirement. `failures/` holds staged
failures that are not yet promoted or whose promotion failed.

For a judge run, `<run-id>/cwd/` is its private directory with the staged
`input`, `output`, `answer`, `tout` and the judge's `report` (execution
contract §2.1).

`<run-id>` is `r` + the five-digit admission index (report contract §3.4);
`<build-id>` is the build's run id.

Builds live inside their job (every test rebuilds, design §6.6), so nothing
is shared between jobs.

### 6.2 Admission and lifetime

The workspace lock is held **continuously** from step 1 to step 5:

1. Lock; leftover recovery (§4.5).
2. Prune steps 1–2 (§6.3): select and rename candidates to `.reclaim-*`.
2a. Allocate the admission sequence: classify `job-seq` (§9);
   `missing` or `malformed` → admission fails closed ("run `cpf fsck
   --apply`"); `unreadable` → fails closed naming the I/O error ("fix access
   to `.cpforge/job-seq`, or remove it and run `cpf fsck --apply`");
   `too_new` → fails closed ("needs a newer cpf"); `next = u64::MAX` →
   refused (`SequenceExhausted`). Otherwise atomically replace it with `next + 1` (§4.2); the
   job's sequence is `(epoch, next)`.
3. Create `runs/<job-id>/` (`create_dir`, fresh id).
3a. Publish `job.toml` no-clobber with the sequence.
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
its own directory creation) and in `cpf clean` (steps 1–3 directly).
`cpf clean` plans by default and with `--apply` prunes with a **zero**
retention budget (`keep_jobs = 0`, `workspace_bytes = 0`): every inactive
job directory is a candidate except newer-schema ones (§9, reported);
active ones are skipped by the lease probe. In the initial feature release it also deletes
`.cpforge/cache/`, and the unreferenced submission
versions (submission contract §11.1). The plan mode takes the lock and so performs the
universal §4.5 leftover recovery, but selects and renames nothing.
It never touches anything else.

1. Under the workspace lock, measure (§6.4) and list inactive job
   directories oldest first (by job id, §6.2). Active jobs count towards the
   measured usage and are never candidates; `keep_jobs` counts inactive
   jobs. A job directory holding a **newer-schema** `job.toml`, `spec.json`,
   `record.json` or `report.json` is never a candidate (reported); one whose
   `job.toml` is missing or malformed (interrupted admission) is a normal
   candidate; a record that cannot be read leaves the directory in place
   (reported), since its schema is unknown (this
   includes `submit/external.toml`, and a `too_new` one is newer-schema). Candidates: beyond the newest `keep_jobs`
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
   the no-follow walker (§2). Before deleting a
   `.reclaim-<job>` that holds `submit/external.toml`, remove the external
   test directory it records per submission contract §7.4 (only a `valid`
   record whose proven marker, a no-follow regular file, holds its token and
   job id; an unmarked directory is kept; an `unreadable` record keeps the job out of
   the candidates in step 1). Leftovers stay as `.reclaim-*` and are retried by the
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
  each allocation (captures, frozen input copies and clones, the staged
  input in `cwd/` and the extracted output file `out/output` of file-based
  I/O runs (execution contract §§5.4, 5.6), judge staging copies and
  session transcripts (execution contract §5.4), specs, records, reports,
  logs, the capture manifest and the report reservation (2 × the report
  bound of the plan, report contract §3.7), and their temp files during
  replacement), with checked arithmetic; released when the file is removed or
  when the job's owner is released. Reflinks count at full logical length.
- **Release after confirmed removal.** A stress job
  removes each retired iteration's scratch with the no-follow walker (§2)
  and, only when the walker confirms the whole removal, releases the
  retained bytes it committed for the removed files (the interface's
  release-after-removal call; stress contract §6.3 R10–R11). A partial
  removal releases nothing and stops the job (stress contract §6.4).
  Files moved into a staged saved failure keep their retained bytes until
  the promotion commits; then their bytes, and those of copied programs,
  are released, since a published saved failure is a user file outside
  every budget (stress contract §7.5).
- **Child output:** after each run, `cwd/` beyond `cwd_bytes` (*default*
  256 MiB) is cleared and `CwdOverflow{bytes}` recorded (diagnostic, not a
  verdict). For a judge run, `cwd/` is measured only after the judge's
  report and `tout` were read and its staged copies removed; for a
  file-based I/O run, only after the output file was collected and the I/O
  scratch removed (execution contract §§2.1, 5.6). Staged files never count
  towards `cwd_bytes`. After each build, output beyond the build cap (configuration contract) →
  `BuildOutputTooLarge`, the build directory is removed (a failed removal is
  reported) and nothing runs from it.

### 6.5 Run facts

- One fact per (problem, solution target): `.cpforge/facts/<problem>/
  <target>.toml` (`schema`, `epoch`, `n`, job id, finish time, verdict
  summary, `source` and `tests` fingerprints, workspace design §3.2 encoding,
  the job's capture check, §7, and `judge`: the judge fingerprint, 64
  lowercase hex characters, configuration contract §9.1.3, absent for a
  problem without a role; an absent field is valid and means none). The
  verdict summary takes every job-verdict token of report contract §2.7.
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

A stress job freezes no tests (its frozen manifest is
empty) and captures the solution target and every role as below and in
C §9.1.3. Its iteration inputs are generated during the job (the
generator's stdout capture, stress contract §3.3) and are tool-created
run files, not frozen copies; nothing in `problems/<p>/tests/` is read. A
replay freezes the saved input (verified against the manifest) as
`inputs/replay/input` and, for `--replay-binaries`, the preserved
programs as `inputs/bin/<role>/` by the same copy rules (regular files
only, exclusive creation, counted in the job budget); a failure is a
capture failure (class `workspace`, exit 8).

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
- **Source capture**. A job's **admission is complete
  only after its freeze**: the §6.2 workspace-lock section (steps 1–5) ends
  with the lease held and the lock released; the freeze then runs outside
  the lock: tests (above), then the selected solution target's **captured
  files** (its `sources` followed by its `support_files`, configuration
  contract §6.2; build planning target resolution) copied into `inputs/src/` with their
  problem-relative paths (same copy rules, regular files only, counted in
  the job budget). Each destination file is created **exclusively**
  (`create_new`); an existing destination (a filesystem alias the plan's
  same-file check missed, e.g. after a rename between plan and capture) →
  capture error `SourceCollision{path}`, nothing overwritten. Then each
  declared role's captured files are copied into `inputs/roles/<role>/`
  with the same rules (configuration contract §9.1.3); the judge
  fingerprint is streamed from the copied bytes. No build or run starts
  before the freeze is complete; a capture failure starts none.
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
- **Artifact test** (submission contract §6.6): the
  guard above is applied to the artifact test's source-consuming stage
  with a path operand inside the job's `inputs/` also rejected, and its
  build and run stages must use the run's own `cwd`. Sources there name the
  external test directory, which is outside the workspace.
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
  a sibling file. How a test whose answer is omitted is judged is decided
  by `[check] answer` (configuration contract §9.1.4): omitted means
  deliberately none, a zero-byte file is legitimately empty, and an
  `answer` naming a missing file is an unhealthy entry.
- No hashes for `custom` tests: the current file is the content, and job
  admission hashes the bytes it copies (§7). Download hashes arrive with
  fetch and mean "content at download time".
- **Download fields** (tool fields, written only by fetch,
  §5.8):

  ```toml
  [fetch]
  txn = "20261123101500123-0123456789abcdef"   # last committed fetch
  upstream_samples = 3                         # upstream pair count at that fetch
  max_ordinal = 3                              # highest upstream ordinal ever seen (§5.8.5)

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
  requires `txn`, `upstream_samples` and `max_ordinal` (each 0–64). Any violation is
  `malformed` (§9), like every invalid known field. Two entries with one
  ordinal or equal hashes are **not** a catalog error: they make a refetch
  a conflict (§5.8.5). The hashes describe the content at download time; an
  entry whose files differ is *edited* and is still run as it is now.
  Fetched answers are always present; a zero-byte fetched answer is
  present-empty.
- Per-test checker overrides are a later design. The key `check` of a
  `[[test]]` entry is reserved for them and is the recognised
  execution-affecting key: an entry holding it (any value) is preserved by
  every edit and is not a catalog error, and a judged job (`cpf test`,
  `cpf interactor-test`, `cpf submit-prep` stage mode) that selects such
  an entry is refused with a configuration planning error, "unsupported in
  this version", exit 7, before admission; it is never ignored. Every
  other unknown key of an entry stays preserved and inert. This refusal
  is a gate-2 addition: the gate-1 catalog reader drops unknown entry
  keys, so until it lands a selected entry with `check` is judged by the
  problem-level settings.
- **Classification** (§9 user-editable): `missing` (no file: an empty
  catalog; `add` and `register` create it), `unreadable`, `malformed`
  (including any invalid known field or duplicate), `too_new` (raw display
  only), `valid`. Only `missing` and `valid` permit mutation; the others
  refuse every command but `ls`, which reports the classification and still
  lists discovery candidates. `cpf fetch` refuses at step
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
  `.cpf-tmp-*` names are never candidates. Superseded
  downloads and unregistered upstream pairs (§5.8.5) are ordinary
  candidates: listed, never run, registrable by `cpf test register`.
The CLI foundation implements it over `tests.toml` + `tests/`. Fetch keeps that model: no generations; the catalog edit of §5.8 is
the commit. Lock-free readers of metadata tolerate absence,
replacement between reads and malformed hand edits (reported with location);
captures, logs and build outputs are running artefacts until their job
finalizes them (execution contract §6.2).

## 9. Schema versions

- `problems/<p>/stress/failures/<id>/failure.toml`
  (stress contract §7.3): schema 1, tool-written once and then user-owned;
  read with the tool-only record classification below (unknown keys
  `malformed`, absent `schema` `malformed`), never mutated by cpf in any
  classification; a `too_new` manifest is listed raw and its failure is
  never replayed.
- Governed formats: `cpforge.toml`, `problem.toml`, `tests.toml`,
  `contest.toml` (user-editable, ordered `[[problem]]` membership, §5.2);
  `state.toml`, `spec.json`, `record.json`, `report.json`, `job-seq`, `job.toml`, `facts/*.toml` (tool-only).
  Also `.cpforge/fetch/<p>/journal.toml`, `status.toml`
  and `txn-<txn>.toml` (tool-only, schema 1, unknown keys `malformed`, each
  field set of §5.8 owned by its writer). The `tests.toml` download fields
  (§8.1), the `problem.toml` tool fields and `contest.toml` `[contest]`
  (§5.8.13) are additions to schema 1 of those user-editable formats: a
  version without fetch preserves them as unknown keys.
- `problem.toml` `[check]` is not decoded by the record classification;
  configuration validates it (configuration contract §9.1).
- The `problem.toml` organisation fields and the
  `contest.toml` top-level `tags` (§5.9.1) are additions to schema 1 of
  those formats; a version without them preserves them as unknown keys.
  Their state (`absent`, `valid`, `invalid{reason}`, §5.9.2) is decided by
  their own field rules **after** classification and never changes the
  record's classification: an invalid organisation field leaves the record
  `valid`, selectable and executable. `notes.md` is free text, not a
  governed format: no schema, no cap except the 64 KiB that search reads
  (§5.9.8).
- **Archive records**: `.cpforge/archive/<txn>/journal.toml`
  and an archive root's `.cpforge-archive/root.toml` are tool-only TOML
  records (schema 1, unknown keys `malformed`, classified as below, 1 MiB
  cap). Snapshot manifests, summaries, contest intents and member markers,
  cleanup records and quarantine records are JSON documents with their own
  bounded codec (archive contract §4.4), not TOML records, so the cap below
  does not apply to them. A `too_new` archive record of either kind is never
  acted on, rewritten or removed. `[problem] origin` is an addition to
  schema 1 of `problem.toml`, preserved as an unknown key by versions
  without it.
- **Fetch records**: classified with the tool-only record
  classification below. Newer-schema handling: a `too_new` journal blocks
  `cpf fetch` of its problem and is never removed; a `too_new` history
  record is kept, never rewritten and contributes no provenance; a
  `too_new` status is not replaced. Size: every fetch record, and the
  catalog after a fetch edit, is checked against the cap before it is
  written, and so is `problem.toml` after the metadata edit, rendered
  against its current bytes (§5.8.4 step 2, before anything is published):
  exceeding it is the refusal `record_too_large`, never a truncation. A
  `problem.toml` that grows past the cap between step 2 and step 6 is
  `metadata_not_applied{too_large}` after the tests are committed (§5.8.8),
  never a retried failure. With at most 64 pairs (§5.8.1) and cpf-chosen
  names, the fetch records stay far below the cap.
- `.cpforge/submit/<p>/current`, each version's
  `manifest.toml` and a job's `submit/external.toml` (tool-only, schema 1,
  unknown keys `malformed`, record classification below; submission
  contract §§7.2, 8.3, 8.4). A `too_new` `current` is never replaced, a
  version with a `too_new` manifest is never pruned, a job directory with a
  `too_new` external record is never pruned.
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
  and `job-seq`, `facts/*.toml` and the fetch records, one enum used everywhere: `missing` (no file);
  `unreadable` (the bytes cannot be read: I/O error, permission, not a
  regular file); `malformed` (not TOML; `schema` absent or not an integer;
  `SchemaTooOld`, none exists yet; or a supported schema with a missing or
  invalid field); `too_new` (`schema` newer than supported: nothing else is
  decoded); `valid`. Steps in order: read bytes; parse TOML; read `schema`
  alone; only for the supported schema decode and validate the fields. A
  file is never decoded as the current schema before its `schema` is
  checked.
- **Encodings** in these records and in `job.toml`: `epoch` is a
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
| `job-seq` replace (admission 2a) | old counter | counter advanced; no job dir yet = an unused number (gap) |
| `job.toml` publish (3a) | job dir without `job.toml`: interrupted admission | job dir with sequence, lease not yet locked: interrupted, reclaimable |
| fact replace (§6.5) | previous fact | new fact, even if the lease was never released |
| fact publication with another epoch or unavailable `job-seq` | previous fact, `NotPublished` reported | — (no commit) |
| `init` `job-seq` (§5.1) | no `job-seq`, no `cpforge.toml`: rerun is initial | `job-seq` without `cpforge.toml`: rerun keeps it |
| `init` `cpforge.toml` (§5.1) | valid `job-seq`, no `cpforge.toml`: rerun keeps the sequence and publishes config | complete workspace; rerun keeps both files |
| `fsck` new epoch | `job-seq` missing or malformed, admission refused | new epoch; jobs of old epochs publish no fact |
| `job-seq` `unreadable` (permission removed) | admission refused naming the error; a finishing job publishes `NotPublished{SequenceUnavailable}`; `fsck --apply` reports, does not repair | after access is restored: admission continues the same epoch |
| fact classification | valid TOML with supported schema but no `n`, or `n = "01"`: `malformed`, replaced on publication; newer-schema fact lacking current fields: `too_new`, preserved by publication and by `fsck` for a deleted problem | — |
| adopt / gitignore | old content | new content |
| `tag`, `untag`, `mark` edit (§5.9.4) | record byte-identical to before; temp may remain (recovered by the next locked operation) | record with exactly the new organisation values and every other byte unchanged |
| `notes` publish (§5.9.5) | no `notes.md`; temp may remain (recovered) | empty regular `notes.md` |
| every fetch boundary | see the fetch crash matrix below | |
| saved-failure staging (`stress.failure_staged`) | nothing under `problems/<p>/stress/failures/`; the staged directory in the inactive job directory, pruned with it | — |
| parents created (`stress.failure_parents`) | empty `stress/failures/` (user tree, kept) | — |
| saved-failure publish (`dir_noclobber.renamed`, `stress.failure_published`) | no `<id>/` under `failures/` | `<id>/` complete: `failure.toml` valid and every listed file matching its hash; nothing of it left in the job directory |
| iteration scratch removal | the iteration's directories partly present in the inactive job directory, pruned with it | — |
| snapshot publication (archive contract §5.2) | workspace unchanged; root staging may remain (removed at the next root lock) | complete snapshot; workspace unchanged |
| retirement rename (archive contract §5.3 step 3) | journal `retiring`, problem in place: rolled back by the next locked operation | problem in `.cpforge/archive/<txn>/retired`: side trees, selection and facts rolled forward by the next locked operation |
| each cleanup move, the trash call, the quarantine record (archive contract §6) | item in the retired tree or the bundle; cleanup recovery settles it by where it is | item in trash, quarantine or deleted; recovery continues with the next |
| contest manifest (archive contract §5.6) | incomplete contest snapshot, no member retired | complete contest snapshot |
| restore publication (archive contract §7.3) | nothing in `problems/`; staging and the transaction removed by recovery | problem published; history rolled forward |
| quarantine purge rename | entry intact | `.purge-<txn>` deleted by the next locked operation |
| version publish (submission contract §5.3 step 5) | no version; staging (source copy) or the job's `submit/version/` (stage) may remain | version complete and unreferenced until `current` is replaced; pruned by `clean` |
| `current` replace | old `current` (or none); temp may remain (recovered) | `current` names the new version |
| external test directory (submission contract §7.3) | see the submission crash table (submission contract §13) | |

**Fetch crash matrix** (the C5 gate; C6, C7 and C8 for
their scenarios).

*Scenarios*, each with a fixed initial workspace `W(S)` and parsed problem:
**C** creation with three pairs; **R** refetch with one `updated`, one
`added` and one `kept_edited` pair and a title change (`P = 6` planned files
in publication order); **M** metadata-only refetch (title change, `P = 0`);
**A** refetch of a problem whose `tests.toml` is missing (three `added`
pairs, `P = 6`, schema-only creation in step 5); **X** as R, with the 4th
planned file (answer of the second pair) created by the test at
`fetch.before_publish#4` as a user file `U` with other bytes (`name_taken`,
`k = 4`; a cell killed before that barrier never injects `U`); **K** contest
of four members.

*References*: `Ref(S)` is the workspace after an uninterrupted run of `S` on
`W(S)`; for X it is the aborted workspace (`failed` record listing files
1…3, `U` present and unrecorded, catalog and `problem.toml` as in `W(X)`).
`Ref(R)` serves as X's successful reference where a cell needs one (X's
crash cells before the barrier leave R's states). "Equal to `Ref`" means
byte-equal after substituting the txn id (in the record's `txn`, `[fetch]
txn` and each `download.txn`). `Next(cell)` is the workspace after an
uninterrupted `cpf fetch` of the same parsed problem, collision injection
disabled, started on a copy of the cell's recovered workspace (taken at the
`fetch.after_reconcile` barrier).

*Crash points.* Fetch-scoped points count only what their name says:
`fetch.before_publish#i` (planned file `i`'s temporary file written, before
the key recheck and the publishing call) and `fetch.file_published#i` (after
its publication and directory sync) count planned files 1…P;
`fetch.catalog_validated` and `fetch.metadata_validated` sit after the last
validation of steps 5 and 6, before the rename (the temp exists). The one
primitive point used is `file_noclobber.linked` (Linux link fallback): every
cell starts a fresh process on a workspace without a journal, so the journal
is that process's first no-clobber file publication and planned file `i` is
occurrence `#(i+1)`. Reconciliation points are the same names under
`fetch_reconcile.`.

*Reconcilers.* (a) `cpf fetch` with the same parsed problem, observed at the
barrier `fetch.after_reconcile` (step 0, after reconciliation, before the
new area is created): the cell's assertions are taken there; then the
barrier is released and the **follow-up** fetch is asserted separately.
(b) `cpf clean` (reclaims the area, does not reconcile), then `cpf fsck
--apply`; assertions after it. Row 24 uses (a) only, asserted after the
complete rerun.

| # | Crash point | Scenarios | Left by the kill | Reconciliation | Expected after it |
|---|---|---|---|---|---|
| 1 | `fetch.area_created` (directory, no lease) | C R M A X | area without `lease` | area reclaimed | none |
| 2 | `fetch.prepared` | C R M A X | area with a free lease | as 1 | none |
| 3 | `fetch.downloaded` | C R M A X | area with samples | as 1 | none |
| 4 | `fetch.revalidated` | C R M A X | as 3, no journal, no status | as 1 | none |
| 5 | `fetch.journal_published` | C R M A X | journal, nothing published | M: rolled forward; C: rule 7; R A X: rolled back (present set empty) | M: committed; others: none |
| 6 | `fetch.before_publish#i`; R A: i = 1…6; X: i = 1…3 | R A X | journal, files 1…i−1, a temp in `tests/` | temp removed by recovery; rolled back | rolled back with files 1…i−1 (none for i = 1) |
| 7 | `file_noclobber.linked#(i+1)`, i = 1…6 (Linux) | R | files 1…i, temp hard-linked to file i | temp removed, file i keeps its bytes; i < 6: as 8; i = 6: as 9 | as 8 or 9 |
| 8 | `fetch.file_published#i`; R A: i = 1…5; X: i = 1…3 | R A X | files 1…i | rolled back | rolled back with files 1…i |
| 9 | `fetch.file_published#6`, `fetch.files_published` | R A | every planned file | rolled forward | committed |
| 10 | `fetch.catalog_created` | A | as 9 plus `schema = 1\n` | rolled forward, the file reused (§5.8.4 step 5) | committed |
| 11 | `fetch.catalog_validated` | R M A | temp beside `tests.toml`, catalog old | temp removed; rolled forward | committed |
| 12 | `fetch.catalog_committed` | R M A | committed, phase `"files"` | rule 5: step 6, step 7 | committed |
| 13 | `fetch.metadata_prepared` | R M | phase `"metadata"`, `problem.toml` as in `W` | rule 5: `metadata_not_applied{interrupted}`; step 7 | committed, `problem.toml` as in `W` |
| 14 | `fetch.metadata_validated` | R M | as 13, temp beside `problem.toml` | temp removed; as 13 | as 13 |
| 15 | `fetch.metadata_committed` | R M | `problem.toml` = after | rule 5: step 6 done; step 7 | committed |
| 16 | `fetch.history_published` | C R M A | record and journal | rule 1: status, journal removed, no edit | committed |
| 17 | `fetch.status_replaced` | C R M A | as 16, new status | as 16 | committed |
| 18 | `fetch.journal_unlinked` (before the directory sync) | C R M A | no journal, free area | area reclaimed | committed |
| 19 | `fetch.journal_removed` | C R M A | as 18 | as 18 | committed |
| 20 | `fetch.area_renamed` | C R M A | `.reclaim-<txn>` | deleted by recovery | committed |
| 21 | `fetch.staged`, `dir_noclobber.staging_synced` | C | journal `create = true`, staging | staging removed; rule 7 | none |
| 22 | `dir_noclobber.renamed`, `fetch.problem_published` | C | problem published, journal | rule 5 (no step 6), step 7 | committed |
| 23 | `fetch.abort_recorded` | X | `failed` record (files 1…3) and journal | rule 2: journal removed, no second record | aborted (`failed`, files 1…3) |

*Kills inside reconciliation* (reconciler (b) killed at a
`fetch_reconcile.` point, then a second `cpf fsck --apply`). The gate is
**every** pair of a row/scenario/`i` cell above and a reconciliation point
that its reconciliation passes, derived from the path it takes:

| Reconciliation path (rows above) | Points it passes, in order |
|---|---|
| roll forward with a metadata diff: rows 7 (i = 6), 9, 11 of R; row 5 of M; row 11 of M | `catalog_created` (row 9 of A only), `catalog_validated`, `catalog_committed`, `metadata_prepared`, `metadata_validated`, `metadata_committed`, `history_published` |
| roll forward without a metadata diff: rows 9, 10, 11 of A | `catalog_created` (row 9 only), `catalog_validated`, `catalog_committed`, `history_published` |
| rule 5, phase `"files"`: row 12 of R and M; row 12 of A | R, M: `metadata_prepared`, `metadata_validated`, `metadata_committed`, `history_published`; A: `history_published` |
| rule 5, phase `"metadata"` or creation: rows 13, 14, 15 of R and M; row 22 of C | `history_published` |
| roll back with F non-empty: rows 6 (i ≥ 2), 7 (i < 6), 8 | `rolled_back_recorded` |
| rule 1, rule 2, rule 7, roll back with F empty, no journal: rows 1–4, 5 (C R A X), 6 (i = 1), 16–21, 23 | none (status and journal removal only) |

Expected after the second reconciliation: the first reconciliation's
expected state of that row, except that a kill at `metadata_prepared` or
`metadata_validated` gives the row-13 state (`metadata_not_applied
{interrupted}`, `problem.toml` as in `W`). A kill at `history_published`
makes the second reconciliation follow rule 1: catalog, `problem.toml`,
`tests/` and the record are the row's expected state (rows 13 and 14:
`problem.toml` as in `W`), and the status is rule 1's (`committed`,
`recovered = true`) with no finding required; in-memory findings of the
first reconciliation, such as `metadata_not_applied`, are not persisted and
need not reappear.

| # | Crash point | Scenario | Rerun (reconciler (a) only) |
|---|---|---|---|
| 24 | `fetch.member_finished#2`, `fetch.contest_created`, `fetch.membership_edited` | K | every member committed; membership equal to `Ref(K)` |

*Expected states*, compared in every cell together with: no journal, nothing
in `downloads/` or `staging/`, no `.cpf-tmp-` name; every file that was under
`tests/` in `W(S)` byte-unchanged and still present (no test file is
modified or deleted); every other file of `W(S)` unchanged unless the
expected state below names it (the catalog, `problem.toml` and
`status.toml`); and the status outcome with `recovered = true` wherever a
journal was reconciled.

- **none**: catalog and `problem.toml` as in `W(S)`; no history record.
- **committed**: catalog, `problem.toml`, `tests/` and the history record
  equal to `Ref(S)` (row 13: `problem.toml` as in `W(S)`).
- **rolled back with files F**: catalog and `problem.toml` as in `W(S)`;
  `tests/` = `W(S)` plus exactly F (with `Ref(R)`'s, `Ref(A)`'s bytes for R/X,
  A); history record `rolled_back` with that reference's `fetched_at`,
  `url` and `[[pair]]`, and as `[[file]]` exactly F with its paths and
  hashes, role `unreferenced`; no record when F is empty.
- **aborted** (row 23 of X): catalog, `problem.toml`, `tests/` (including
  `U`, which is never recorded, never registered and byte-unchanged) and the
  `failed` record equal to `Ref(X)`'s; no second record. The status is
  asserted separately: rule 2 gives `rolled_back` with `recovered = true`
  (`Ref(X)`'s live status is `failed`).

*Follow-up* fetch of reconciler (a) (collision injection disabled): after
committed → `unchanged` (byte-identical apart from `status.toml`); in every
other case the result equals `Next(cell)` (txn substituted), and in addition,
independently of `Next`: after row 13/14 it is a committed metadata-only txn
with `problem.toml` equal to `Ref(S)`'s (S = R, M); after none, rolled back
or aborted it is a committed txn whose pair outcomes, file hashes and roles
equal `Ref(R)`'s (R, X), `Ref(A)`'s or `Ref(C)`'s, whose new paths are the
ones §5.8.6 chooses given the files left (skipping every name of F and `U`),
and whose `added` entries have `id` equal to their chosen stem (so an id may
differ from the reference's, e.g. `sample-3.r2` when `sample-3.in` was left
by the rollback); every other entry field equals the reference's. A second
`cpf fsck --apply` after any cell changes nothing. Every cell runs on Linux,
macOS and Windows except row 7 (Linux only). The tables list the gated cells;
the reconciliation-kill cells are exactly those derived above.

**Guarantee scope.** The matrix tests level 2 (§1): after a process kill the
OS keeps every completed write, rename and unlink. Power loss (level 3) stays
best effort and untested; what the implementation does for it, per OS:
Linux and macOS sync each published file before its rename (macOS with
`F_FULLFSYNC`, best effort) and the parent directory and newly created
ancestors after it (§4.3), and sync `.cpforge/fetch/<p>/` after the journal
unlink; Windows flushes each published file (`FlushFileBuffers`) before its
rename and flushes no directory, so no rename or unlink is made durable by
cpf there. On no OS are the download area's bytes, its creation, its
reclamation (rename to `.reclaim-<txn>`, deletion) or staging deletion
synced. After a power loss a removed journal may therefore reappear, or a
file named by a later record may be missing; §5.8.7 still decides such a
state (a reappeared journal with a `committed` record is finished; a missing
planned file makes a not-committed journal roll back), but no test covers
it and no outcome is promised.

Coverage assignment (command rows remain assigned to their implementing tasks):

| Rows / transaction | Task coverage |
|---|---|
| atomic replace, shared-record edit, file and directory publication primitives; lock/recovery boundaries | C4b (covered by library helper subprocesses) |
| init, init job-seq (including primitive publication and lock/recovery boundaries) | C4c (covered) |
| add directory + membership (including staging, publication, contest creation/edit and lock/recovery boundaries) | C4d (covered) |
| test add, register, enable/disable (§5.3) | C6 |
| job admission, prune, job-seq replace, job.toml, fact replace/publication/classification, unreadable job-seq | C10/C15 tasks |
| fsck new epoch; adopt; gitignore | their respective command tasks |
| `tag`/`untag`/`mark`, `notes`, filters, `ls`, `status` with filters, `search`, `created_at` | the organisation command tasks (fetch creation's `created_at`: the fetch creation task); archive-root search with the archive |
| every crash table and test of the archive contract (§10 there) | the archive tasks: plan and classes; root guard and codec; snapshot publication; retirement; cleanup and trash; recovery; quarantine; restore validation, `--into`, containment and publication; contests; archived listing and search |
| every cell of the fetch crash matrix, the §5.8.8 failure transitions, reconciliation, download-area reclamation, readers, records | C5 (transaction, recovery, matrix rows 1–23 for R, M, A, X), C6 (creation, row C cells), C7 (pair rules, provenance), C8 (selections, bulk bound, row 24), C9 (status surfaces) |

Reruns: `init`, `add --contest` complete the operation; `test add` may
create a new number and leave the earlier files unregistered.
A rerun of `cpf fetch` reconciles first (§5.8.7), then fetches; a rerun of a
contest fetch completes its membership.

**Tests:**

- `SG-01` Kill a real subprocess at every commit point (test-only env var); assert
  the allowed states and the rerun behaviour. Storage primitives and edit
  transactions use a library helper subprocess (C4b). Each command task
  additionally kills the actual `cpf` binary at that command's commit points.
- `SG-02` Injected failures: link + failed unlink; rename + failed directory sync
  (`Durability::Uncertain`, caller continues); directory `RENAME_NOREPLACE`
  unsupported; `EXDEV` on publish; existing empty destination directory;
  disk full; permission denied; temp removal failure before commit.
- `SG-03` Edit transaction: in-place edit, atomic editor save, deletion, type change
  and malformed TOML between steps; Windows sharing retries with a save
  between attempts; two `cpf` processes on one file; hard link refused; mode
  preserved; untouched-item round-trip.
- `SG-04` Composition: nested operations never re-acquire the lock; leftover recovery
  runs before the current transaction creates files.
- `SG-05` `test add`: `.ans` `Exists` after `.in` published, and an editor modifying
  the `.in` before that: the `.in` is kept, reported unregistered; a
  registered entry whose `tests/custom-1.in` is missing makes `add` choose
  `custom-2`; an editor taking the id or a path during the replay → conflict,
  catalog unchanged.
- `SG-06` Test commands (C6): `register` uses an existing sibling `.ans` and says
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
- `SG-07` Leases and pruning: held lease never pruned; killed job becomes prunable;
  directory without lease reclaimed; unreadable lease left and reported;
  rename refused (Windows: a handle open inside the job directory; injected
  elsewhere) leaves the directory; `.reclaim-*` leftovers retried; the
  probe is released before the rename (Windows: the rename succeeds with no
  handle open and fails, reported, with the owner's lease open); `keep_jobs` and byte-target triggers;
  newer-schema job metadata never pruned.
- `SG-08` Retention: per-job budget refusal (execution contract); `CwdOverflow`;
  `BuildOutputTooLarge` removes the build directory; a mount point inside a
  job directory is skipped by measuring and deletion.
- `SG-09` Shared `cwd`: never measured or cleared; a file-based I/O problem with a
  shared `cwd` is refused (configuration contract §11).
- `SG-10` Freeze: edits between tests and between repeat iterations do not change
  later runs; an edit and an injected interruption **during** freezing (the
  run does not start; the manifest is never partial); freeze failure starts
  no run; FIFO refused.
- `SG-11` Admission: a second `cpf` pruning concurrently never selects a job being
  admitted (lock held through lease creation); `cpf clean` racing admission.
- `SG-12` **Judge fingerprint in facts** (real jobs on Linux and macOS; on
  Windows the fact codec and the status comparison are run as pure tables
  over constructed facts): a judged `cpf test` with a `simple` checker
  publishes a fact whose `judge` equals the report's `fingerprints.judge`;
  `cpf status` then gives target `judge` `unchanged` and the human
  "unchanged"; one byte of the checker source changed → `judge` `changed`
  with `comparison` `unchanged`, human "judge changed"; the `[check]`
  `scoring` scale changed → `changed`; only `display` changed →
  `unchanged`; a problem without roles → a fact without `judge` and
  `judge` `none`; a fact with `judge` against a problem whose roles were
  removed, and a fact without it against a problem that gained a checker →
  `changed`; a `malformed` fact → `unknown`. Golden vectors: the fact of a
  fixed job (clock and job id injected) and its `status --json` document
  are byte-equal to committed golden files, one pair per OS. Kill at the
  fact replace (real `cpf`): before the rename → the previous fact
  byte-identical and `status` compares with it; after it → the new fact
  complete with its `judge`.
- `SG-13` Mount boundaries: a bind mount of the same filesystem inside a job
  directory (Linux, privileged CI step) is skipped by measurement and
  deletion.
- `SG-14` Per-job accounting: reservation before each copy, replacement temp counted,
  release on removal, overflow of checked arithmetic refused.
- `SG-15` Links in tool-written paths refused; root via a link works.
- `SG-16` Schema refusal per format; answer absent vs present-empty.
- `SG-17` **Fetch harness.** A test adapter (`storage-test-hooks`)
  returns a given selection (§5.8.1) without network; every fetch test below
  drives the actual `cpf` binary with it. Assertions read back, after each
  step: the catalog (entries in order, `download` tables, `[fetch]`), the
  `problem.toml` tool fields, the files of `tests/` with their SHA-256, each
  history record (outcome, pairs, files and roles), the status outcome and
  findings, the journal (presence, phase, fields), and the entries of
  `downloads/`. Crash points: those of the crash matrix above. Pause
  barriers: `fetch.network` (area held, no lock), `fetch.before_publish#i`
  (temporary file written, before the key recheck and publishing call of
  planned file `i`), `fetch.before_catalog_read` (files published, before the
  step-5 edit reads the catalog), `fetch.before_validation` (step-5 document
  rendered, before the first validation and rehash), `fetch.after_rehash`
  (after the last step-5 rehash, before the rename), `fetch.before_metadata`
  (before the step-6 `prepare`), `fetch.before_history`, `fetch.before_unlink`,
  `fetch.after_reconcile` (step 0, after reconciliation, before the area is
  created).
- `SG-18` **Fetch crash states:** every cell of the crash matrix (row × scenario ×
  `i` × reconciler), with its expected state and follow-up; one test per
  cell, named by row, scenario, `i` and reconciler.
- `SG-19` **Journal completeness:** for R killed at `fetch.files_published`, and R
  and M killed at `fetch.catalog_committed`, reconciler (b) runs with the download area
  already reclaimed and the test adapter disabled (any adapter call fails the
  test): the history record is byte-identical (except txn) to the
  uninterrupted run's, including `fetched_at`, `url`, pair outcomes and the
  `superseded` hashes; for a rolled-back X, the record lists exactly the
  present set.
- `SG-20` **Restart cases** (after the kill, before reconciliation):
  - `tests.toml` edited (an `enabled` change) after `fetch.files_published`
    → rolled back;
  - a planned file modified after `fetch.files_published` → that file is not
    in the history record and the others are `unreferenced`;
  - a planned file name created by the user with other bytes → user
    content, not recorded;
  - `cpf test register` of a published planned file, and separately `cpf test
    add`, after `fetch.files_published` → catalog base changed → rolled back;
    the registered file's provenance is `registered`, the others
    `downloaded{unreferenced}`;
  - a `problem.toml` tool field edited after `fetch.catalog_committed` →
    kept, `metadata_conflict{field}`;
  - after `fetch.history_published`, `fetch.metadata_committed` and
    `fetch.metadata_prepared`, `problem.toml` (a) restored to its exact
    `W(S)` bytes, (b) title edited to a third value, (c) left alone →
    after reconciliation `problem.toml` is byte-identical to what the user
    left in every case; the finding is none for `history_published`, and
    for the other two `metadata_not_applied{interrupted}` except
    `metadata_committed` (c) (step 6 done). The follow-up refetch then applies
    the per-field rule to the current values;
  - the catalog edited (an `enabled` change) after `fetch.catalog_committed`
    → steps 6–7 still rolled forward, catalog untouched by reconciliation;
  - for A killed at `fetch.catalog_created`: reconciliation reuses the
    schema-only file (no `Exists`), commits, `tests.toml` equal to
    `Ref(A)`; with that file edited by the user (a comment added) →
    base mismatch, rolled back;
  - the catalog made `malformed` after `fetch.catalog_committed`, and
    separately after `fetch.files_published` → journal kept, `fetch` refused,
    `fsck` and `status` report `undetermined`; after the user repairs it,
    reconciliation finishes (first case) or decides by the bases (second);
  - a file planted as `txn-<txn>.toml` at `fetch.before_history` →
    `history_blocked`, journal kept, `fetch` refused; after it is moved away
    a fetch reconciles and writes the record.
- `SG-21` **Failure transitions** (§5.8.8), one test per table row, injected with
  the primitive test hooks (permission denied, sync failure, cleanup
  failure) at the step: assert the committed state, journal, history and
  status of that row, then that the next reconciliation reaches the row's
  stated successor. Named cases: a second hard link to `problem.toml`
  created at `fetch.before_metadata` → `metadata_not_applied
  {not_single_file}`, journal removed, both names byte-unchanged; the same
  at `fetch.catalog_committed` with reconciler (b) → the same finding; one
  test per permanent refusal kind of §5.8.4 step 6 that a test can provoke
  (`not_single_file`, `too_large`, `invalid`, `unbound`); abort-history
  publication failing (permission denied on `.cpforge/fetch/<p>/`) after a
  transient step-5 error with every planned file present and bases
  unchanged → `abort_unrecorded`, journal kept, and after access is restored
  the next reconciliation **commits** (rule 6); the same after a step-5
  `Conflict` from an editor save → the next reconciliation rolls back; a
  file planted as `txn-<txn>.toml` before an abort records → `history_blocked`
  (rule 3). Windows, with real sharing violations (a handle opened
  without delete or write sharing, held by the test): on `tests.toml` through
  step 5 → after the ≤ 2 s backoff `failed`, catalog unchanged, history
  `failed`, and a rerun after release commits; on `problem.toml` through step
  6 → `metadata_pending`, journal kept, a fetch after release reconciles
  (rule 5); on `journal.toml` at `fetch.before_unlink` → `journal_pending`,
  and a fetch after release removes it by rule 1 with no catalog or metadata
  edit.
- `SG-22` **Catalog baseline** (§5.8.4 step 5): at `fetch.before_catalog_read` an
  editor deletes an `updated` entry; changes its `input` path; disables it;
  converts it to `custom` (removes `download`) → each `conflict`
  `base_changed`, catalog byte-identical to the editor's version, history
  `conflict` with the published files `unreferenced`. For A, a `tests.toml`
  created by the user at that barrier → `conflict`, the user's file kept.
- `SG-23` **Aborts:** at `fetch.before_publish#i` another process creates the
  planned name, separately for an input and an answer: exact spelling →
  `Exists`; a case variant (`Sample-1.R2.ans`) → the key recheck on every
  OS; the name `sample-1.r2.in` when the plan chose `r2` (an occupied
  revision name) → each `name_taken`, history `failed` listing exactly files
  1…i−1 as `unreferenced`, file `i` the user's bytes and unrecorded, no file
  after `i`; a rerun chooses the next free revision. A hash mismatch
  injected into the area → `failed`, the file not published. An editor save
  of a replaced input at `fetch.before_validation` → `conflict`
  `edited_during_commit`, catalog byte-identical, published files
  candidates; the Windows retry path with a save between attempts → the same
  conflict.
- `SG-24` **Observation points and the race:** a save of a download entry's input at
  `fetch.network` → `base_changed`, nothing published, no journal; a save at
  `fetch.after_rehash` → the catalog names the revision pair, the superseded
  file keeps the user's bytes, `cpf status` reports it as edited (the
  stated race); a save to `problem.toml` at `fetch.before_metadata` →
  per-field rule.
- `SG-25` **Pair rules:** one test per row of the §5.8.5 outcome table, each with an
  enabled and a disabled entry; input-only edit, answer-only edit, deleted
  input, deleted answer, input replaced by a directory; each ambiguity kind
  (`duplicate_ordinal`, `duplicate_recorded`, `duplicate_upstream`, `moved`
  by swap and by insertion at the front) → conflict, workspace
  byte-identical except `status.toml`; identical upstream with duplicate
  pairs → `unchanged`; zero samples with and without `--disable-removed` →
  set kept, `zero_samples`; `removed_upstream` with `--disable-removed` →
  disabled, files untouched; `removed_locally` → not re-added; **shrink and
  regrowth**: three samples, entry 3 deleted locally, upstream two, then
  three again (and then two, one, three) → entry 3 never re-added,
  `max_ordinal` stays 3, `upstream_samples` follows the count; the same with
  entry 3 disabled instead → it stays disabled and is matched again on
  regrowth; `max_ordinal` hand-lowered below an entry's ordinal → treated as
  that ordinal; three refetches with an edited pair and an unchanged upstream
  revision → exactly one unregistered upstream pair; an unchanged refetch →
  every file byte-identical except `status.toml`, no history record.
- `SG-26` **Provenance:** after a committed update (journal removed), the old pair
  is `downloaded{superseded}` and the new `registered`; after three
  successive updates of one pair, each earlier pair is `superseded` and the
  counts by role match the records; with history records listed in any
  directory order, and with a txn id that sorts before an earlier one (clock
  rollback, injected id), the roles are the same; with an open committed
  journal and with it removed, the same roles; a superseded file edited by
  the user → `unknown`; a superseded file registered by `cpf test register`
  → `registered` (custom), history records unchanged.
- `SG-27` **Names:** `sample-1.r2.in` present and unregistered → `r3`; a directory
  named `sample-1.r2.ans` → `r3`; a catalog path naming a missing
  `sample-1.r2.in` → `r3`; a history-listed name deleted by the user is not
  reused; `added` on an existing unregistered `sample-1.in` → `sample-1.r2`
  with id `sample-1.r2`; collision-key variants (`Sample-1.R2.in`) count as
  taken.
- `SG-28` **Download area:** at `fetch.network`, `cpf fsck --apply`, `cpf clean
  --apply` and `cpf clean` from other processes leave the area and its
  lease intact and the fetch then commits; a kill at `fetch.network` → the
  next locked command removes the area; an area without `lease` → removed;
  an unopenable lease (Unix permission) → kept and reported by `fsck` and
  as `RecoveryIncomplete`; an unexpected entry in `downloads/` → kept and
  reported; the owner's own recovery at step 2 sees its area `held`;
  Windows: owner release renames with no handle open.
- `SG-29` **Concurrency:** two fetches of one problem paused at `fetch.network`,
  released in turn → the first commits, the second `base_changed` with
  nothing published; a rerun → `unchanged`. A fetch and `cpf test add` on
  one problem serialize by the lock; a `test add` between steps 0 and 2 →
  `base_changed`. A job admitted before a refetch and paused during its
  freeze (`inputs.source_copied`, and between two test copies) while the
  refetch commits → the job's test ids are those of the catalog it resolved,
  and each frozen input and answer is byte-identical to the file that
  catalog named (files are never modified, so no capture is mixed; §7 still
  permits different moments); a job admitted after the commit freezes the
  revision pair.
- `SG-30` **Records:** each fetch record in each classification (`missing`,
  `unreadable`, `malformed`, `too_new`, `valid`): a `too_new` journal
  blocks `cpf fetch` and survives `fsck --apply`; a `malformed` journal is
  removed by `fsck --apply` and its files become `unknown`; a journal with an
  unknown key or without `[history]` → `malformed`; a `too_new` status is not
  replaced; a `too_new` or `malformed` history record contributes no
  provenance; a history record of another identity contributes none;
  download-field validation (`download` on `custom`; without `answer`; a
  missing key; `ordinal` 0 and 65; uppercase or short hash; invalid `txn`;
  `[fetch]` without `txn` or without `max_ordinal`) → `malformed`; a version
  without fetch support preserves the fields as unknown keys through `test
  enable`; a plan whose catalog would pass 1 MiB → `record_too_large`,
  nothing published.
- `SG-31` **Readers:** with a `pending`, a `committed`, a `finished` and an
  `undetermined` journal, `cpf status` and `cpf test ls` report
  `fetch_incomplete` with that phase and leave the workspace byte-identical;
  a pending journal's files are candidates and never run.
- `SG-32` **Prepared edit** (§4.4 prepared form, library helper subprocess as C4b):
  on a CRLF `problem.toml` with comments, the prepared output bytes equal the
  bytes finally published and their SHA-256 equals the journal's
  `after_sha256`; a save between `prepare` and `publish` → `Conflict`, file
  keeps the save, no temp left; on Windows, a save between retry attempts →
  `Conflict` against the retained baseline; dropping a prepared edit leaves
  the directory byte-identical; an *unchanged* result publishes nothing.
- `SG-33` **Metadata:** an unavailable field is not written; a limit change with a
  contest record `limits` override → only `problem.toml` changes,
  `contest.toml` byte-identical. Size: a `problem.toml` padded (a user
  comment) to within 1,000 bytes of 1 MiB and a title growing from one to
  1,024 bytes → for M and for R refused `record_too_large` at step 2,
  nothing published, no journal; the same padding added at
  `fetch.before_metadata` → R `committed` with `metadata_not_applied
  {too_large}`, journal removed, `problem.toml` unchanged.
- `SG-34` **Creation:** an existing local problem whose name is the derived one →
  refused, nothing created; a directory bound to another remote identity →
  `IdentityMismatch`; zero samples → empty catalog with `upstream_samples =
  0`, `max_ordinal = 0` and `no_samples`; non-ASCII sample bytes published
  exactly.
- `SG-35` **Remote identities** (pure tables): accepted `atcoder:abc400_a`,
  `atcoder:a`, an AtCoder native id of 56 characters, `cses:1`,
  `cses:1068`, `cses:9999999`, with the directories `atcoder-abc400_a`,
  `atcoder-a`, `atcoder-` + the 56 characters (64 characters, an
  Identifier), `cses-1`, `cses-1068`, `cses-9999999`; the contest
  `atcoder:abc400` with the directory `contests/atcoder-abc400`. Rejected:
  `atcoder:`, `atcoder:ABC400_A`, `atcoder:_a`, `atcoder:-a`, `atcoder:a.b`,
  a 57-character AtCoder native id, `cses:0`, `cses:01`, `cses:12345678`,
  `cses:a`, `Atcoder:abc400_a`, `1x:a`, a site of 33 characters. Binding:
  `problems/atcoder-abc400_a/problem.toml` with `id = "atcoder:abc400_a"`
  is bound; with `"atcoder:abc400_b"` or `"cses:1068"` →
  `IdentityMismatch`. Local names with remote-looking spellings stay valid
  (§2.1: `cpf add` does not reserve them): `problems/cses-1068` holding
  `id = "local:cses-1068"` and `problems/atcoder-abc400_a` holding
  `id = "local:atcoder-abc400_a"` are valid local problems, selectable as
  `cses-1068` and `local:cses-1068`; the remote references `cses:1068` and
  `atcoder:abc400_a` to those directories fail (another identity is
  bound there, exit 8), and fetch creation of those identities is refused
  (SG-34). Then through the actual
  binary on a workspace whose remote problem and contest the fetch harness
  created: `cpf status atcoder:abc400_a` and `cpf fsck` report no identity
  finding; after the `id` is edited by hand to `"atcoder:abc400_b"`, both
  report `IdentityMismatch` for `problems/atcoder-abc400_a` and no
  reference resolves to it.
- `SG-36` **References** (§5.7): a workspace with `problems/atcoder-abc400_a`,
  `problems/cses-1068` and `problems/two-sum` (all bound), the contest
  `contests/atcoder-abc400/contest.toml` with records `A` →
  `atcoder:abc400_a` and `B` → `atcoder:abc400_b` (no such directory), and
  a local contest `contests/c/` with the record `A` → `local:two-sum`.
  `cpf current --problem <ref>` prints exactly the identity and a newline
  on stdout and nothing else, exit 0, for `atcoder:abc400_a`,
  `atcoder-abc400_a`, `atcoder-abc400/A`, `cses:1068` and `c/A` (→
  `local:two-sum`); `cpf switch atcoder-abc400/A` stores `current_problem =
  "atcoder:abc400_a"`. Each of `atcoder-abc400/a` (labels by exact
  spelling), `atcoder-abc400/B` (the named problem missing),
  `atcoder-abc400/Z` (no record), `nope/A` (unknown contest),
  `Atcoder-abc400/A` (another spelling), `atcoder:abc400_c` (no directory)
  and `atcoder:ABC400_A` (outside the grammar) → exit 8, stdout empty,
  stderr naming the reference and the cause, workspace byte-identical. A
  `contests/c/contest.toml` with two records labelled `A` → `c/A` exits 8
  naming `contest.toml` as `malformed`; `contests/C` beside `contests/c`
  (Linux) → `c/A` exits 8 (several entries); `contests/l` a link → 8.
  `cpf status`, `cpf tag` and `cpf archive` (plan) given the same
  references act on the same problems or fail the same way.
- `SG-37` **Selections and bulk bound:** a `ProblemSet` of 50 members → 50
  transactions, no contest file; 51 members → `selection_too_large{51}`,
  no area, record or contest file created; 51 members with `--only` naming
  50 → accepted; an unknown `--only` label → refused before any member; a
  `Contest` of 51 → refused likewise. (One enumeration request and zero
  member requests on refusal are asserted by the adapter tests, C4/C8.)
- `SG-38` **Contest membership:** a kill at `fetch.member_finished#2` of four, then a
  rerun → membership equals the enumeration in order; a member failing in
  the rerun but committed earlier → listed; a member never committed → not
  listed; insertion before the first later member; an existing record with
  another label keeps it; a label held by another record → record without
  label, reported; `[contest] id` mismatch → refused; a problem shared by
  two contests is one directory listed in both.
- `SG-39` **Fetch exit statuses** (the test adapter returns, per member, either a
  parsed problem or the given sites class): one problem committed,
  unchanged, or committed with only `edited_sample_kept` (an edited pair
  whose upstream changed) → 0; each ambiguity kind (`duplicate_ordinal`,
  `duplicate_recorded`, `duplicate_upstream`, `moved`), `base_changed` (a
  save at `fetch.network`) and `edited_during_commit` (a save at
  `fetch.before_validation`) → outcome `conflict`, 9; a committed refetch
  with `metadata_conflict{title}`, and one with `metadata_not_applied
  {too_large}` → 9; a problem refused at step 0 (`tests.toml` `malformed`)
  → 8; the problem failing with `not_found` → 12. Contest K of four
  members: all committed → 0; member 3 failing `not_found`, the others
  committed → 9; member 2 unchanged and members 1, 3, 4 failing
  `network{status}` → 9; every member failing with a sites class → 12;
  member 2 refused at step 0 and the others committed → 8; member 2
  refused and member 3 failing → 8; member 2 ending `conflict` and member
  3 failing → 9; the membership edit failing after the members committed
  (permission denied on `contest.toml`) → 9. A `ProblemSet` of three with
  one failing member → 9. In every case the `fetch` document's per-member
  `outcome` and `failure.class` are the ones scripted.
- `SG-40` **Fetch history**: a problem with a `committed`, a `rolled_back`, a
  `conflict` and a `failed` history record, one `malformed` and one
  `too_new` record → `cpf fetch --history <p>` lists exactly six records,
  each by its txn, the four `valid` ones with their outcome and the other
  two with their classification, then the latest attempt of `status.toml`;
  exit 0; the workspace is byte-identical, no `.cpforge/lock` is created
  where there was none, and the test adapter is never called. With no
  records → nothing listed, exit 0; outside a workspace → 8.
- `SG-41` **Status fetch member**: `cpf status --json` gives `fetch` = `null` for a
  local problem; `record` `missing` with every other member `null` for a
  remote problem without `status.toml`; after each attempt outcome
  (`committed`, `unchanged`, `conflict`, `refused`, `failed`, and
  `rolled_back` with `recovered = true`) `record` `valid` and exactly that
  record's `attempted_at`, `outcome`, `txn` (`null` where the attempt
  wrote no journal) and `recovered`; `status.toml` `malformed`, `too_new`
  and `unreadable` (Unix permission) → that `record`, the other members
  `null`; with a journal in each phase (`pending`, `committed`, `finished`,
  `undetermined`) → `incomplete` that phase, `null` without a journal.
  After a committed update the superseded pair is in `diagnostics`, and
  the human output shows the counts by role and the latest txn, equal to
  those of the Provenance row's fixtures; an edited pair kept beside its
  unregistered upstream pair lists both. The documents of a committed
  fetch and of a pending journal equal `status_fetch.json` and
  `status_fetch_incomplete.json`. The workspace is byte-identical after
  every `status`.
- `SG-42` **fsync order** (trace recorder, `storage-test-hooks`; it checks call
  order only, not durability): for every fetch publication, file sync before
  rename, and on Linux and macOS rename before directory sync; the journal's
  directory sync before the first test-file publication; the last test
  file's directory sync before the catalog rename; the step-6 journal
  replacement before the `problem.toml` rename; the history record's
  directory sync before the journal unlink; the journal unlink followed by
  its directory sync (Linux, macOS). Windows records file flushes before each
  rename and no directory flush.
- `SG-43` Every fetch test runs on Linux, macOS and Windows (fetch starts no
  process).
- `SG-44` **Organisation harness.** Every test drives the
  actual `cpf` binary. A `storage-test-hooks` clock override fixes "now"
  (`2026-11-24T10:00:00.000Z` unless a row says otherwise); a bounds
  override lowers the search bounds E and B; pause barriers
  `org.after_resolve` (target resolved under the lock, before the first
  read), `org.before_validation` (edit rendered, before the first §4.4 step-4
  validation), `notes.before_publish` (temp written, before the no-clobber
  call) and `search.before_action#k` (before the k-th counted action);
  crash points `org.edit_validated` (temp validated, before the rename),
  `org.edit_committed` (after the rename, before the directory sync),
  `notes.before_publish`, `notes.published`. "Byte-identical" compares
  every file of the workspace (path, bytes, and for records the file
  identity) before and after; "no write" additionally asserts the record's
  file identity and modification time are unchanged. Every row runs on
  Linux, macOS and Windows unless it names an OS.
- `SG-45` **Grammars** (pure tables): tags `dp`, `DP`, `2-sat`, `a`, a 32-character
  tag accepted; `""`, 33 characters, `-x`, `_x`, `a b`, `a.b`, `a/b`, `é`
  rejected. Status names `new`, `in-progress`, `a`, 32 characters accepted;
  `In-progress`, `-a`, `a-`, `1a`, `a_b`, 33 characters rejected. Valid
  `tags` fields: `[]`, 64 tags; invalid: `"dp"`, `[1]`, `["dp", "DP"]`
  (reason names element 2), 65 tags. Timestamps accepted:
  `2026-11-24T10:00:00.000Z`, `2026-11-24T10:00:00Z`, `2026-11-24`,
  `2024-02-29`, `1970-01-01`, `1970-01-01T00:00:00.000Z`,
  `9999-12-30T22:00:00.999Z`, `9999-12-31` (date); rejected:
  `9999-12-30T22:00:01.000Z`, `9999-12-31T00:00:00Z`,
  `9999-12-31T23:59:59.999Z`, `1969-12-31T23:59:59.999Z`,
  `2026-02-29`, `2026-13-01`, `2026-11-31`, `T24:00:00Z`, `T10:60:00Z`,
  `T10:00:60Z`, `+01:00` offset, lowercase `z`, `.12Z`, no `Z`, a TOML
  date-time value, `1969-12-31`, year `10000`. Effective status: absent →
  `new`, `"5"` invalid → none. Filter ranges under the zone seam `UTC0`:
  `2026-11-24` contains `T00:00:00.000Z` and `T23:59:59.999Z` and not
  `2026-11-25T00:00:00.000Z`; `..2026-11-24`, `2026-11-24..`,
  `2026-11-20..2026-11-24` inclusive at both ends; `2026-11-25..2026-11-24`,
  `..`, `2026-02-29`, `2026-11-24T10:00Z` rejected.
- `SG-46` **Local-day tables** (zones passed to the evaluator through jiff's API
  on every OS, built with `TimeZone::posix` from the strings below; the
  end-to-end rows repeat them through `TZ` on Linux and macOS with the same
  expected results): `CET-1CEST,M3.5.0,M10.5.0/3` → `2026-11-24T23:30:00.000Z` is
  local 2026-11-25, `2026-07-01T22:30:00.000Z` is 2026-07-02,
  `2026-07-01T21:59:59.999Z` is 2026-07-01; `EST5EDT,M3.2.0,M11.1.0` →
  `2026-11-25T04:59:59.999Z` is 2026-11-24 and `T05:00:00.000Z` 2026-11-25;
  on 2026-03-08 (23-hour day) and 2026-11-01 (25-hour day) every instant
  from local 00:00 to the next local 00:00 matches that day and no other;
  a midnight-transition rule `<-03>3<-02>,M9.1.6/24,M4.1.6/24` → the day
  after the spring-forward begins at local 01:00 and the instant one
  millisecond before it is the previous day; at the fall-back the repeated
  23:00–24:00 hour belongs to the earlier day and the following midnight
  instant to the new day. A date-only stored `2026-11-24` matches
  `--solved 2026-11-24` under every zone above and is never converted.
  TZif rows (`TimeZone::tzif` with the fixture files): `America/Sao_Paulo`
  → `2018-12-01T02:30:00.000Z` is local 2018-12-01 (00:30 at −02, DST then
  in force) and `2026-12-01T02:30:00.000Z` is 2026-11-30 (23:30 at −03, DST
  abolished since 2019); `Europe/Berlin` `2100-07-01T22:30:00.000Z` →
  2100-07-02 (beyond the TZif transitions, from its POSIX footer). Domain
  edges: under `<+14>-14`, `9999-12-30T22:00:00.999Z` has local date
  9999-12-31 and matches `--solved 9999-12-31`; under `<-12>12`,
  `1970-01-01T00:00:00.000Z` has local date 1969-12-31 and matches
  `--solved ..1970-01-01` but not `--solved 1970-01-01`. Discovery:
  `TZ=Nowhere/Invalid` (Unix) → one stderr warning and UTC-day results;
  `TZ=/<fixture>/Europe/Berlin` and `TZ=:/<fixture>/Europe/Berlin` give the
  Berlin rows; a command without a date filter under the invalid `TZ`
  prints no warning. Rule-3 decision (constructed structures, every OS):
  DST disabled and `wMonth` 0, `Bias` −60, `StandardBias` 0 → fixed +01:00;
  DST disabled with transition dates → jiff lookup; dynamic DST → jiff
  lookup; `Bias` 300, `StandardBias` 0, disabled, `wMonth` 0 → fixed
  −05:00; `Bias` −60, `StandardBias` −30, disabled, `wMonth` 0 → fixed
  +01:00 (`StandardBias` ignored). Windows CI: zone set by `tzutil /s "W. Europe Standard Time"` →
  local dates equal those of the `Europe/Berlin` fixture for the CET rows;
  `tzutil /s "W. Europe Standard Time_dstoff"` → `2026-07-01T22:30:00.000Z`
  is 2026-07-01 (fixed +01, 23:30). Both restore the host zone afterwards.
- `SG-47` **Argument errors before the workspace:** run outside any workspace, each
  of `cpf tag p 'a b'`, `cpf tag p`, `cpf mark p Solved`, `cpf mark
  contest:c solved`, `cpf --problem a tag . dp`, `cpf ls --tag 'a b'`,
  `cpf ls --contests --status new`, `cpf status --solved 2026-02-29`,
  `cpf search` (no term), 17 terms, a 257-byte term, a non-UTF-8 term
  (Unix), `cpf notes contest:c`, `cpf tag p dp --json` exits 2 (not 8),
  and no file is created anywhere.
- `SG-48` **`tag`/`untag` edits:** on a `problem.toml` with CRLF line endings,
  comments, an unknown key and a `[config]` table, `cpf tag p dp graphs`
  → exit 0 and the record equals the expected fixture bytes: only `tags =
  ["dp", "graphs"]` and `updated_at = "2026-11-24T10:00:00.000Z"` added to
  `[problem]`, CRLF kept. Then: the same command → no write; `cpf tag p
  DP` → no write; `cpf tag p Graphs x x` → only `x` appended, once;
  `cpf untag p GRAPHS` → `graphs` removed, `updated_at` at the new clock;
  `cpf untag p nope` → stderr note, exit 0, no write; untagging the last
  tags → `tags = []`. An existing multi-line `tags` array with a comment
  keeps the comment and layout after an append.
- `SG-49` **`mark`:** `cpf mark p solved` → `status`, `solved_at`, `updated_at` all
  equal the clock; again at a later clock → no write; `cpf mark p
  in-progress` (clock T2) → `status` changed, `solved_at` unchanged,
  `updated_at` = T2; `cpf mark p solved` at T3 → `status = "solved"`,
  `updated_at` = T3, `solved_at` still T1. A hand-written `status =
  "solved"` without `solved_at` → `cpf mark p solved` writes nothing;
  `cpf mark p new` then `cpf mark p solved` at T4 → `solved_at` = T4. With
  workspace `status.values = ["wa"]` → `cpf mark p wa` succeeds; without →
  7, record byte-identical, message lists the five-name vocabulary; user
  `["wa"]` with workspace `["x"]` → `wa` is 7 (the array replaces);
  `status.values = [1]` → 7; a phase-1 error in a language table only →
  `mark` succeeds with a warning. `mark p new` on a problem without
  `status` writes `status = "new"`.
- `SG-50` **Workflow status untouched by jobs:** on a problem with `status =
  "in-progress"` and on one without `status`, a `cpf test` in which every
  test passes, a `cpf stress --iterations 3` that finds no failure and a
  `cpf submit-prep --source-copy` (Linux and macOS) → `problem.toml`
  byte-identical (no `status`, `solved_at` or `updated_at` written) and
  `cpf status` still shows `workflow: in-progress`, respectively `new`. On
  Windows the same `cpf test` command line exits 10, `--source-copy`
  exits 0, and `problem.toml` is byte-identical.
- `SG-51` **Invalid fields block only their writers:** `tags = "dp"` → `cpf tag`
  and `cpf untag` exit 8 naming `tags`, record byte-identical, and `cpf
  mark p solved` succeeds (it does not write `tags`); `updated_at = 5` →
  `tag`, `untag` and `mark` exit 8; `solved_at = "x"` → `mark p solved` 8,
  `mark p skipped` succeeds. In every case `cpf current --problem p`,
  `cpf switch p` and `cpf status p` still select the problem (the record is
  `valid`); `cpf status --json` keeps the raw `tags` / `user_status`
  members exactly as before this amendment and lists the invalid field in
  `findings`; `cpf ls --json` gives `[]` / `null` with the finding.
  Regression: the existing `status` fixtures and gate-1 status tests pass
  with only the added `findings` member.
- `SG-52` **Tag count:** 63 tags + `cpf tag p a A b` → `a` added once, `b` → 65 →
  refused 8 `too_many_tags`, byte-identical; 63 tags + `cpf tag p a A` →
  64, committed; 63 tags, an editor adds the 64th at `org.before_validation`
  of the first attempt, `cpf tag p x` → the replay counts 65 → 8, the
  record equals the editor's version.
- `SG-53` **Binding on replay:** at `org.before_validation` an editor changes
  `[problem] id` from `local:p` to `local:q` (a valid identity) → the
  replay refuses with `IdentityMismatch`, exit 8, the record equals the
  editor's; the same on the first attempt (the change made at the barrier
  `org.after_resolve`, between resolution and the first read); a contest
  whose `[contest] id` an editor adds at the barrier → 8. **Fetch overlap:**
  `cpf tag p dp` while a refetch of `p` is paused at `fetch.network` → the
  tag commits, the fetch ends `conflict` `base_changed` (problem base),
  nothing published, and a rerun of the fetch commits with the tag kept
  byte-identical; `cpf tag p dp` started while a refetch holds the lock
  (paused at `fetch.before_metadata`) waits, then commits on the fetch's
  result with both the fetch's metadata and the tag present.
- `SG-54` **Targets:** `.` from inside `problems/p/` tags `p`; `.` with a saved
  selection and the root as cwd tags the saved problem; `.` with no
  selection → 8 `NoSelection`; a bare name naming only a contest → 8 with
  the contest note, `contest.toml` byte-identical; `cpf tag contest:c
  rated` adds top-level `tags = ["rated"]` to a local `contest.toml`, every
  `[[problem]]` record byte-unchanged and no `[contest]` table created; a
  `malformed` `contest.toml` → 8, byte-identical.
- `SG-55` **Editor and concurrency:** at `org.before_validation` an editor saves
  `problem.toml` by atomic rename adding a comment → the replay commits and
  the record holds both the comment and the tag; an editor saving at the
  barrier of both attempts → 8, the record equals the editor's second
  version, no `.cpf-tmp-` file left; the same with an in-place save. Two
  `cpf tag` processes on one problem with different tags, the first paused
  at the barrier → the second waits for the lock, and afterwards both tags
  are present in order of commit. Windows: a handle without write or delete
  sharing held for 3 s → 8, record unchanged; released after 0.5 s → the
  retry commits.
- `SG-56` **Crash states:** kill at `org.edit_validated` → record byte-identical to
  before and one `.cpf-tmp-` file beside it; the next `cpf tag` removes it
  (leftover recovery) and commits. Kill at `org.edit_committed` → record
  equals the expected new bytes. Both for `tag`, `untag`, `mark`.
- `SG-57` **Size:** a `problem.toml` padded by a comment to 1 MiB − 10 bytes →
  `cpf tag p dp graphs` 8 (`record_too_large`), byte-identical.
- `SG-58` **Clock outside the domain:** each case starts from a fresh
  fixture in which every command would write: problem `p` with `tags =
  ["x"]` (no `dp`) and no `status`, and no `problems/q`. With the clock
  override at `9999-12-30T22:00:01.000Z`, and separately at
  `1969-12-31T23:59:59.999Z`, each of `cpf tag p dp`, `cpf untag p x`,
  `cpf mark p solved` and `cpf add q` → 8 naming the clock, the record
  byte-identical, no `problems/q`, no `.cpf-tmp-` file. At
  `9999-12-30T22:00:00.999Z`, and separately at
  `1970-01-01T00:00:00.000Z`, each command on its own fresh fixture
  commits and writes exactly that value: `updated_at` for `tag`, `untag`
  and `mark`, also `solved_at` for `mark`, and `created_at` (not
  `updated_at`) for `add`.
- `SG-59` **Creation dates:** `cpf add two-sum` → `[problem] created_at` equals the
  clock in the published record; at each existing `add` crash point the
  published problem, when present, has it; `cpf add --contest c two-sum`
  on the existing problem leaves its `problem.toml` byte-identical.
  (Fetch creation: with the fetch creation tests.)
- `SG-60` **`updated_at` writers:** on problems with `updated_at` absent and with
  `updated_at = "2026-01-01T00:00:00.000Z"`: a committed metadata refetch
  (a title change, scenarios R and M of the fetch harness), `cpf adopt`, `cpf
  test add`, `register`, `enable`, `disable` and `cpf notes` → an absent
  `updated_at` stays absent and a present one keeps its bytes; the next
  `cpf tag p x` sets it to the clock.
- `SG-61` **`notes`:** absent → stdout exactly the absolute path and `\n`, stderr
  `created`, a 0-byte regular file; again → `exists`, no write; an existing
  file with content → unchanged; `notes.md` a directory, a symlink (target
  untouched), a FIFO (Unix, returns without blocking) → 8, stdout empty; an
  editor creating `notes.md` with bytes `x` at `notes.before_publish` →
  exit 0, `exists`, content `x`, no temp left; two `cpf notes` started
  together → both exit 0 with the same stdout, one file created; kill at
  `notes.before_publish` → no `notes.md`, a temp the next locked command
  removes; kill at `notes.published` → empty `notes.md`. `--path` in a
  workspace without `.cpforge/lock` → no lock file created, nothing
  created, exit 0 for absent, existing and directory `notes.md`, stderr
  naming each state. `cpf status` never lists `notes.md` as untracked.
  `--json`: document vectors for `created`, `existing`, `absent`.
- `SG-62` **Filters (fixture workspace F):** twelve valid problems covering status
  absent, `new`, `in-progress`, `solved`, `skipped`, `wa` (not in the
  vocabulary) and `5` (invalid); tags `dp`, `DP`, `graphs`, none and the
  invalid `"dp"`; sites `local` and `atcoder`; dates in all three
  accepted forms, one invalid, some absent; two contests sharing a member,
  each with contest tag `rated`. One table of at least 16 filter sets
  (each flag alone, repeated `--status` (OR), repeated `--tag` (AND),
  `--tag DP` = `--tag dp`, `--tag rated` matching no problem, `--contest`
  OR, each date flag with each range form, combinations) with the expected
  identity list per set. For every set, `cpf ls` and `cpf status --all`
  (and `cpf status` without a selector) give exactly that list in that
  order, human and `--json`; `--status new` includes the absent one;
  `--status 5`-style invalid values are 2, and the invalid-status problem
  appears in no `--status` result and in every listing's findings.
  `cpf status contest:c1 --status solved` → the solved members in
  `contest.toml` order. `--contest nope` → 8.
- `SG-63` **Invalid entries:** F plus `problems/broken/` with a malformed
  `problem.toml`, a link `problems/l`, and a regular file `problems/f` →
  `cpf ls` exit 0 listing all three under `invalid` with their reasons and
  the twelve problems as before; `cpf status --all --tag dp` → 8 naming
  `broken`, as without the filter.
- `SG-64` **Contest status:** a contest of five members (two `solved`, one without
  `status`, one with an invalid status, one whose directory is missing) →
  `cpf ls --contests` shows `solved 2, new 1, invalid 1, unresolved 1`;
  after `cpf mark` of the `new` member to `solved` → `solved 3, …`, and
  `contest.toml` is byte-identical.
- `SG-65` **Contest listing:** contests `atcoder-abc400` (`[contest] id =
  "atcoder:abc400"`, tag `rated`), `c` (local, no `[contest]` table, tag
  `Rated`), `broken` (malformed `contest.toml`) and a regular file
  `contests/f` → `cpf ls --contests` exit 0 listing the two valid contests
  with their tags and member counts and `broken` and `f` with their
  reasons; `--site atcoder` → only `atcoder-abc400`; `--site local` → only
  `c`; `--tag rated` → both valid contests; `--tag dp`, where only a member
  problem has `dp` → none.
- `SG-66` **`ls` exit statuses:** outside a workspace → 8 (with `--json` the
  `error` document); `problems/` unreadable (Unix) → 8; `contests/`
  unreadable with `--contests` (Unix) → 8; `--contest broken` (malformed
  `contest.toml`) → 8; invalid entries and field findings → 0; a closed
  stdout with `--json` → 11; `cpf ls --contests --status new` → 2.
- `SG-67` **Readers:** after each of `cpf ls`, `cpf ls --contests`, `cpf status`
  with filters, `cpf search` and `cpf notes --path` on F, the workspace is
  byte-identical and no `.cpforge/lock` exists where there was none;
  `cpf ls --archived` tests are in the archive contract
  §10.10. Document vectors
  for `ls` (problems and contests) and `search`.
- `SG-68` **Search hits (fixture F with notes):** expected hit lists, with matched
  fields and note line numbers, for at least: `DP` (case), a precomposed
  `é` term against decomposed notes text and the reverse, `e` matching
  `é`, two terms matched in different fields, a term only in notes with and
  without `--no-notes`, a term plus `--status solved`, a term matching only
  a contest tag (no hit), and a term matching nothing (zero hits, exit 0,
  `no matches` and the findings).
- `SG-69` **Search output:** a `notes.md` with the term on 20 lines → the hit
  lists exactly the first 16 line numbers; on 16 lines → all 16. A
  complete search writes no `search incomplete` line; a search ending with
  `byte_limit` and `notes_truncated` → the last stderr line is `search
  incomplete: ` followed by those two reasons in that order; zero hits with
  an unreadable `notes.md` → `no matches`, the `unreadable_source`
  finding, then the incompleteness line.
- `SG-70` **Notes bounds:** a `notes.md` of exactly 65 536 bytes → not truncated,
  complete; 65 537 bytes → `notes_truncated` on that problem and the reason
  in `incomplete`; a term only in byte 65 537 onward → no hit; a 3-byte
  UTF-8 character straddling 65 536 → dropped, no `notes_not_utf8`; a `0xFF`
  byte → still searched, `notes_not_utf8`, no incompleteness from it; FIFO,
  symlink to a regular file, directory → `notes_not_regular` and
  `unreadable_source`, the run finishes; unreadable notes (Unix mode 000,
  Windows deny-read ACL) → `unreadable_source`; `--no-notes` on the same
  workspace → no notes finding and complete.
- `SG-71` **Search bounds** (bounds override; ten problems `p01`–`p10`, each with a
  `problem.toml` of 1 000 bytes and a `notes.md` of 2 000 bytes, every one a
  hit): E = 30 → complete, ten hits, E used 30; E = 24 → seven hits
  (`p01`–`p07`), `entry_limit`; E = 5 → zero hits, `entry_limit`, five
  entries enumerated; B = 30 000 → complete; B = 10 000 → three hits,
  `byte_limit`, B used 10 000 (`p04`'s record read, its notes refused);
  `--no-notes` with B = 10 000 → ten hits, complete. A `problem.toml` grown
  by a writer during the read at the barrier never makes B exceed the
  bound. One real-bound test on Linux: 100 001 empty directories in
  `problems/` → `entry_limit` with E = 100 000 and no file opened.
  **Operation accounting:** a `storage-test-hooks` recorder logs every
  filesystem call the search makes, tagged with the charged operation it
  belongs to (§5.9.8); in every bounds test each call belongs to a charged
  operation, the number of charged operations equals the reported
  `entries` (30 in the E = 30 row: 10 entries + 20 opens), and the byte
  total equals `bytes_read`. `problems/` with spelling
  variants (`P1`, `p1`, Linux) → both reported `name_collision` with no
  second enumeration. A `contests/` of 100 001 entries: `cpf search x` →
  complete, the recorder shows no `contests/` access; `cpf search x
  --contest c` → `entry_limit` during the `contests/` enumeration, no
  problem examined.
- `SG-72` **Cancellation:** SIGINT at `search.before_action#20` (Unix) → hits only
  among the actions before it, `cancelled`, death by SIGINT; with `--json`
  the delivered document has `cancelled`; Windows Ctrl-Break → 6. The
  workspace is byte-identical. **Blocked I/O:** a test hook stalls the
  worker inside its 5th read indefinitely; SIGINT (Unix) or Ctrl-Break
  (Windows) → output starts within 200 ms, the document lists exactly the
  candidates finished before the stall plus `cancelled`, and the process
  exits (death by SIGINT / 6) within 1 s; a second signal ≥ 100 ms after a
  first one that is ignored by an injected stalled stdout → force exit.
- `SG-73` **In-place writers:** with the reader paused after reading the first half
  of `problem.toml` (hook `org.read_partial`) and the file rewritten in
  place, `cpf ls` classifies exactly the bytes read (the expected
  `malformed` finding for the fixture's torn bytes), exits 0 and reads the
  file once; the same for `notes.md` in `cpf search` (searched as read, no
  error).
- `SG-74` **Search exit statuses:** outside a workspace → 8 with the `error`
  document under `--json`; no `problems/` → 0, complete, zero hits;
  `problems/` unreadable (Unix) → 8; closed stdout with `--json` → 11;
  incomplete for each reason except `cancelled` → 0; cancelled with
  `--json` and a closed stdout → 11 (report §5.2 row 4 before row 5); a
  usage error (`cpf search`) with `--json` and a closed stdout → 2.
- `SG-75` **Organisation maintenance:** a workspace with `tags = "dp"`, `status =
  "5"` and `solved_at = "x"` on three problems, a contest with `tags = [1]`,
  a `notes.md` that is a directory and one that is a FIFO (Unix) → `cpf
  fsck` exit 0 reporting each with its problem or contest, field and
  reason, and each non-regular `notes.md`; `cpf fsck --apply` and `cpf
  clean --apply` → every record and `notes.md` byte-identical (with file
  identities).
- `SG-76` **`gitignore` lines:** `cpf gitignore` prints exactly the lines of §5.5
  in that order: the seven active lines ending with `.cpforge/archive/`
  and `.cpforge/quarantine/`, then the comments `# optional:
  .cpforge/facts/`, `# optional: .cpforge/fetch/` and `# optional:
  problems/*/stress/failures/`, and no active line naming `facts/`,
  `fetch/` or `stress/`; `--write` on a fresh workspace publishes exactly
  that block between the markers.
- `SG-77` **Archive.** The archive contract §10 lists the
  archive, cleanup, trash, quarantine, restore, contest and archived-search
  tests, each killing the real `cpf` binary at every crash point it names
  and asserting the recovered content; they run on Linux, macOS and Windows
  (the archive starts no process), with the trash backend of each OS tested
  on that OS and the quarantine-only paths on all three. Added here: after
  each crash cell of those tables, a locked command that is not an archive
  command (`cpf tag`) performs exactly rule R and nothing else (retired and
  quarantined files byte-identical before and after it).
- `SG-78` Real runs on Linux, macOS and Windows on the supported filesystems;
  power-loss behaviour is not tested. Platform floor and toolchain as in
  [README](../../README.md) (platform floor).
