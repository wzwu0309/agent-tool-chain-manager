import type { ProjectSkillRecovery } from "./tauri";

/** Active unfinished transactions are not guaranteed to be interrupted yet.
 * Present them as requiring verification; never call the project usable. */
export const recoveryNeedsAttention = (recovery: ProjectSkillRecovery) =>
  recovery.phase === "needs_attention"
  || recovery.phase === "preparing"
  || recovery.phase === "publishing";

/** A retained backup requiring review is different from a blocked transaction.
 * cleanup_pending is retention metadata, not evidence of broken service. */
export const recoveryNeedsFollowUp = (recovery: ProjectSkillRecovery) =>
  recoveryNeedsAttention(recovery) || recovery.phase === "recovery_available";

export const recoveryMayBeInProgress = (recovery: ProjectSkillRecovery) =>
  recovery.phase === "preparing" || recovery.phase === "publishing";
