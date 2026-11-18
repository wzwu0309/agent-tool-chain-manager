# Issue #11: Recovery Data Cleanup Contract — Candidate

Status: **DRAFT / review required**. This document is a proposed authorization boundary,
**not** a declaration that destructive recovery cleanup is implemented or safe.
Scope: project Skill operation journal and the corresponding
\`<agent-skills-root>/.atm-recovery/<operation-id>\` folders. No generic
folder cleaner or global \`--force\` bypass is authorized here.

## Current verified behavior

- \`project_skill_operations\` persists operation ID, project ID, Skill ID,
  optional DeploymentId, operation kind, phase, original target, recovery root,
  and, for certain operations, destination/observed identity.
- \`preparing\` and \`publishing\` are in-flight; startup recovery conservatively
  attempts a no-replace restoration only where the original target is unoccupied.
  Ambiguous or occupied paths are not replaced.
- \`recovery_available\` and \`needs_attention\` retain potential user data.
  \`cleanup_pending\` also retains previous Copy/quarantined deletion bytes
  **after a successful database commit**. A process can continue writing
  through an already-open descriptor pointing at the renamed old directory.
- CLI, Tauri, and project GUI expose read-only recovery records. No supported
  cleanup command exists. A directory's presence, content match, Git Tree
  match, or \`cleanup_pending\` alone **does not grant disposal authority**.

## Proposed authorization sequence (not implemented)

1. **Project-bound inspection**: resolve the registered ProjectId and exact
   OperationId in the journal. Display kind, phase, target and recovery paths,
   current deployment record, and a non-following inventory of all retained
   files. Never infer ownership from matching content or a central Skill link.
2. **No implicit eligibility**: automatically refuse cleanup of
   \`preparing\`, \`publishing\`, \`recovery_available\`, or \`needs_attention\`.
   \`cleanup_pending\` only permits *considering* a user-requested cleanup;
   it does **not** prove the backup is no longer changing or needed.
3. **Explicit target-bound consent**: require a fresh confirmation whose
   scope includes ProjectId + OperationId + recovery root identity + target
   path + recorded phase. Do not expose a free-form path or global
   \`force=true\` argument as an alternative to authorization. Explain
   explicitly that old open file descriptors can still write into the
   retained recovery directory after cleanup was proposed.
4. **Conservative execution**: recheck under RepoLock that journal stage,
   ProjectId, OperationId, DeploymentId (where present), target and recovery
   paths, recovery root parent, and observed filesystem identities have not
   changed. Reject symlinked recovery roots or parents and path substitution.
   A failed validation makes **no filesystem change**.
5. **No data-loss claim from stat/hash equality**: inode/device, mtimes and
   content hashes can detect many changes but cannot prove that an external
   process has closed its file handles. A future implementation must retain
   the original backup or export it to an independently selected,
   verified user-owned archive *before* irreversible deletion, and document
   the residual adversarial-writer/atomicity limits.
6. **Journaled finalization and recovery**: before any irreversible action,
   persist an authorized intent and a recoverable operation stage; never
   mark a record complete merely because a rename succeeded. On SQLite
   failure, process crash, or newly occupied path, preserve the retained
   data and return a concrete recovery location. Repeat a completed request
   idempotently without applying it to a new object at the same path.

## Required negative evidence before opening a destructive UI/CLI action

| Case | Required result |
| --- | --- |
| User supplies arbitrary path, wrong ProjectId/OperationId | Refuse; no write |
| External path or symlink replaces recovery directory/parent | Refuse; no traversal |
| Backup moved/replaced between preview and confirmation | Refuse; do not delete replacement |
| \`recovery_available\` or \`needs_attention\` | Inspect only; no cleanup |
| \`cleanup_pending\` with old descriptor receiving late writes | Retain original bytes; never claim quiescence from hashes |
| Concurrent project update/toggle/delete | Serialize; stale authorization fails |
| SQLite failure after filesystem step | Preserve backup, journal recoverable state |
| Real SIGKILL between intent, quarantine, and finalization | Restart does not silently destroy retained content |
| Repeat cleanup request after a different object occupies path | Refuse or idempotent no-op; never operate on new object |

## Open decisions

- How long should \`cleanup_pending\` files be retained by default, and may
  the application only **offer archiving** (not deletion) in the first phase?
- What exact review/consent evidence is persisted, and how is the consent
  invalidated when target/recovery identities change?
- Is a user-directed cleanup allowed to explicitly accept the unpreventable
  late-open-descriptor risk, or is deletion prohibited until an independently
  archived copy is confirmed?
- Which long-term recovery operations are guaranteed to be reversible after
  a platform/power failure? Existing SIGKILL tests do not establish power-loss
  durability or directory fsync guarantees.

**Current implementation decision:** read-only inspection is allowed; no
automatic or one-click destructive cleanup is added in Issue #11 until this
contract has been reviewed and the negative tests above are demonstrated.
