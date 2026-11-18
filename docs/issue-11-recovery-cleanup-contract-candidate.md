# Issue #11 · Recovery disposition authorization contract · V2 candidate

**Status:** NEEDS REVIEW — contract candidate, not implementation sign-off.
**Version:** V2 · 2026-11-18.
**Scope:** retained project Skill operation objects in
`<agent-skills-root>/.atm-recovery/<OperationId>/`, as well as their
SQLite journal entries. The existing Git Commit/Tree baseline and deployed
`DeploymentId` remain separate authority domains.

This V2 incorporates the independent review of the original candidate at
commit `2f9b764`. It intentionally **does not authorize permanent deletion**,
schedule automatic expiration, repurpose an existing operation phase as user
consent, or introduce a new CLI/GUI destructive action.

## 1. Frozen first-phase policy

| Action | Authorization and first-phase behavior |
| --- | --- |
| Read-only inspection | Allowed for a registered ProjectId + OperationId; never follows a recovered symlink |
| Export/archival copy | Future **explicitly authorized**, non-destructive operation; source remains untouched |
| Move/rename the source recovery directory | Not in first-phase implementation |
| Release/unpin Git Commit refs | Not a side effect of archival, recovery, or project unregistration |
| Permanently delete source recovery objects | **Prohibited in V2**, regardless of user confirmation, `--force`, or matching hashes |
| Automatic retention timeout / garbage collection | **None**; `cleanup_pending` never implies disposable |
| `recovery_available` or `needs_attention` | Inspect/manual-repair-first; do not treat as cleanup eligibility |
| `cleanup_pending` | Eligible for a future review and non-destructive archival request, **not deletion** |
| `preparing` or `publishing` | In-flight/ambiguous; inspect only until state is resolved |

**Archive is not cleanup.** Copying the source into a user-controlled archive
neither revokes a running process's old file descriptors nor guarantees the
source is quiescent. Even after an archive is verified, the original data and
SQLite operation record remain. Do not label the result "safe to delete".

For first-phase archiving, copy into a **separate staged destination** on the
archive volume and then publish *that staged archive* without overwrite. Do
not implement the source as `rename` and do not implement a cross-filesystem
`copy + delete` surrogate. The source recovery path remains stable and
available to startup/manual recovery throughout.

## 2. Exact object inventory and limits of evidence

A single operation root may contain heterogeneous children:

| Child | Meaning (check actual journal kind/phase before showing) |
| --- | --- |
| `previous` | Old Copy saved before a replacement/removal |
| `new` | Prepared but not yet adopted content |
| `failed-new` | Failed or interrupted published new content |
| `recovered` | Unpublished initial-deploy content preserved after failure |
| Other/unexpected entries | **No implicit inclusion**; re-inspect and require a new explicit scope |

Do not grant a blanket `.atm-recovery/<OperationId>` permission. An
authorization names the **exact selected child objects** and their
descendants as observed during inspection, not the folder and whatever later
appears inside it. If `failed-new` appears after consent to archive
`previous`, the old consent does not cover it.

The read-only inventory should record, for each selected object:

- ProjectId, OperationId, operation kind, SkillId and DeploymentId (if any)
- Source recovery root and each ancestor's non-followed filesystem identity
- Child name and relative descendant paths (no arbitrary caller-supplied path)
- Kind (directory, regular file, symlink, or unsupported special object)
- Per-object `dev/ino` (or platform-equivalent handle-bound identity), mode,
  size, and observed metadata; exact symlink **text** if present, never follow it
- Bounded content inventory and digest scheme/version for change detection
- Journal phase, row version/generation, observation time, selected archive
  destination and action kind

Content hashes, matching `TreeOid`, and identical `dev/ino` at two
instants are **negative evidence filters**, not proof of provenance, absence
of open handles, or permanent quiescence. A complete recursive inventory
must have explicit resource bounds; encountering an unsupported special
file, excessive depth/size, or changing tree is a **refusal**, never a skip.

## 3. Single-use, scoped authorization

A future disposition request must bind all of the following in one stable
authorization descriptor. Its effective scope is the intersection, never the
union, of the recorded facts:

1. `DispositionId` (unique, single-use) and `DispositionKind=ArchiveCopy`
2. ProjectId, OperationId, OperationKind, SkillId and optional DeploymentId
3. Exact journal phase **and a monotonic journal generation** (not merely
   timestamp precision)
4. Project root, recovery parent, recovery root, and selected child
   identities, captured without following unexpected symlinks
5. Exact child names and complete inventoried subobjects/digest version
6. Archive destination path, parent directory identity, and no-clobber policy
7. User confirmation subject, scope, confirmation time, and authorization
   freshness limit

An authorization covers **only** the observed selected objects. No generic
`path`, `force=true`, recursive archive of new children, or fallback from
an unverified identity is allowed. Any change of phase/generation, entry,
parent identity, subtree inventory, or destination revokes the authorization;
re-inspection and fresh consent are required.

The UI must explain that a process can keep writing into `previous` after
archival starts or finishes. "The snapshot currently matches" must never be
rendered as "all writers have stopped".

## 4. Independent disposition journal/state machine (not implemented)

A future `recovery_dispositions` table is distinct from
`project_skill_operations`. The latter continues to govern original
deploy/update/delete/toggle and their restoration. **Never repurpose
`cleanup_pending` to mean user authorization or archival completion.**

Proposed minimum workflow:

```text
Inspected (read-only preview, no reservation)
  -> Authorized (one-shot intent durably recorded)
  -> Archiving (staging in destination; source unchanged)
  -> Verifying (archive inventory and source identity checks)
  -> Archived (verified archive published, source remains retained)

Any uncertain state -> NeedsAttention (no original content deleted)
```

The proposed disposal record stores identity and generation, source/target
objects, authorization hash/nonce, exact archive location, the stage, error,
and timestamps. **Authorized** consumes the user's one-shot token before
physical work begins; retries look up the same `DispositionId` and may
resume the same archive, but must not apply it to a new object or target.

Crash/failure requirements:

- Before creating a staged archive: persist `Authorized` and identity set.
- Before writing staged bytes: persist `Archiving`; stage destination is
  unique, no-clobber and separately identifiable.
- If copying or verification fails: retain source **and** staged archive,
  mark `NeedsAttention`, report concrete recovery locations. Never delete
  inconsistent copies to hide failure.
- If interrupted after archive publication but before SQLite COMMIT: restart
  must match the exact archive identity and contents, then either resume the
  same `DispositionId` or leave `NeedsAttention` — never create a second
  archive at a new path based on the stale consent.
- Only after archive verification and SQLite COMMIT may the UI show
  `Archived`. It still does not permit deleting original recovery bytes.
- Repeating an already `Archived` disposition is a read-only idempotent
  report, not a second copy or a new destructive request.

The exact SQL columns, durability barriers, archive manifest format and
error codes must be separately reviewed before implementing this state
machine. A generic `remove_dir_all` is expressly out of scope.

## 5. macOS-safe path and filesystem operations

`RepoLock` serializes cooperating Manager operations but **does not** stop
unrelated processes from renaming directories or writing through handles.
A one-time `canonicalize` / `symlink_metadata` followed by a string-path
recursive traversal is insufficient for disposition execution.

macOS first-phase implementation requirements:

- Bind each crucial ancestor using directory file descriptors, and use
  `openat` / `fstatat(AT_SYMLINK_NOFOLLOW)` with `O_DIRECTORY`,
  `O_NOFOLLOW` where applicable; never follow an unexpected symlink.
- Operate on children relative to validated directory handles, re-check
  object identity and generation before copy and before archive publication.
- Create unique archive-stage directories under a validated destination
  parent, with exclusive creation and no replacement.
- Publish a fully staged archive using a platform-verified no-clobber
  operation (`renameatx_np(RENAME_EXCL)` where appropriate), and record
  `DispositionId` in its manifest.
- Define file and directory fsync requirements and test them before claiming
  crash/power-loss durability. Do not imply SQLite WAL COMMIT makes arbitrary
  file copies durable.
- If strong handle-relative semantics are unavailable on a platform, fail
  closed; do **not** downgrade to string-path traversal or recursive delete.

Directory identity by `(dev, ino)` is useful for rejecting most swaps but
does not eliminate inode reuse; stable handles and the persisted operation
intent must corroborate it. Archive consistency while an external writer
modifies the source cannot be established by hash alone.

## 6. Lifecycle intersections

### Project unregister

The first code-level stopgap is **block project unregistration while any
journal row has `phase != complete`**, so project-scoped recovery remains
accessible. It must hold the same RepoLock as managed operations and check
the SQLite journal in the same transaction as removal. This applies to
`cleanup_pending` as well as `needs_attention`/`recovery_available`.

The stopgap intentionally cannot solve permanent project detachment while
recoveries remain. A later release must provide an independent
registered/detached recovery index and path-authorized read-only access,
rather than requiring users to keep a project registered forever. Do not
offer a "remove project and forget recoveries" bypass meanwhile.

### Copy ownership

`TreeOid` is proof of content, **not the identity of a managed filesystem
directory**. The v13 Copy identity binding records the manager-published
directory identity, verifies continuity before and after publication and
blocks updates to same-content replacements. Pre-v13 Copy rows lack
continuous object identity and must not be silently rebound during
migration; a separately reviewed explicit reauthorization process is
required. Even strong snapshot identity checks do not eliminate an
uncooperative actor's race, so handle-bound no-replace APIs remain the
future hardening target.

### Git pins

Git Commit pins and recovery object archival are independent lifecycles.
Successful archival, project unregister, or a retention age must **not**
auto-remove a Git ref. A future pin-GC protocol needs its own reference
graph, proof that no managed deployment/recovery requires the commit,
and separate fault-injection tests.

## 7. Required negative tests before enabling archive requests

| Scenario | Required result |
| --- | --- |
| Wrong ProjectId/OperationId/child identity or unregistered project | Refuse; no write |
| Added `failed-new` after `previous` was authorized | Exclude; stale authorization rejected |
| Source child/ancestor replaced with equal-content object | Refuse even if Git Tree matches |
| Symlinked recovery root, child, or archive parent | No symlink traversal; fail closed |
| Journal generation/phase or content inventory changed | Reject, require fresh inspection |
| `recovery_available` / `needs_attention` | Read-only inspection; no archival in phase one |
| External process writes through old descriptor during archive | Preserve original; never claim no live writer |
| Destination occupied or changes during copy/publish | No replacement; retain source and stage |
| SQLite error before/after publish, lost ACK, duplicate request | Idempotent intent handling; no object loss |
| Real SIGKILL between Authorized/Archiving/Verifying/Archived | Recover stage or NeedsAttention; no silent deletion |
| Cross-volume destination | Copy stage in target volume; do not emulate atomic move |
| Project unregister with non-complete journal | Refuse; journal remains accessible |
| Managed Copy replaced by identical-content directory | Refuse update/toggle/delete regardless of content match |
| Old Copy record with unknown filesystem identity | Refuse auto-rebind; explicit future authorization |
| Git pin still referenced when archive completes | Preserve pin unchanged |

**Implementation status:** V2 is a design baseline for independent review,
not authorization to ship archival or deletion. Existing read-only recovery
views continue to work, and permanent recovery deletion remains prohibited.
