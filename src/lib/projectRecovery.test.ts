import { describe, expect, it } from "vitest";
import type { ProjectSkillRecovery } from "./tauri";
import { recoveryNeedsAttention, recoveryNeedsFollowUp, recoveryMayBeInProgress } from "./projectRecovery";

const recovery = (phase: string): ProjectSkillRecovery => ({
  operation_id: "op-1",
  project_id: "project-1",
  skill_id: "skill-1",
  deployment_id: "deployment-1",
  kind: "update",
  phase,
  target_path: "/example/.claude/skills/demo",
  recovery_root: "/example/.claude/skills/.atm-recovery/op-1",
  destination_path: null,
  target_exists: false,
  recovery_root_exists: true,
  retained_paths: ["/example/.claude/skills/.atm-recovery/op-1/previous"],
  created_at: 1,
  updated_at: 2,
});

describe("read-only project Skill recovery alerts", () => {
  it("flags interrupted or possibly still-in-progress operations", () => {
    for (const phase of ["needs_attention", "preparing", "publishing"]) {
      expect(recoveryNeedsAttention(recovery(phase))).toBe(true);
      expect(recoveryNeedsFollowUp(recovery(phase))).toBe(true);
    }
    expect(recoveryMayBeInProgress(recovery("preparing"))).toBe(true);
    expect(recoveryMayBeInProgress(recovery("publishing"))).toBe(true);
    expect(recoveryMayBeInProgress(recovery("needs_attention"))).toBe(false);
  });

  it("shows retained recovery_available for review but not as a service outage", () => {
    expect(recoveryNeedsFollowUp(recovery("recovery_available"))).toBe(true);
    expect(recoveryNeedsAttention(recovery("recovery_available"))).toBe(false);
  });

  it("does not mistake a normal cleanup_pending backup for an outage or recovery task", () => {
    expect(recoveryNeedsAttention(recovery("cleanup_pending"))).toBe(false);
    expect(recoveryNeedsFollowUp(recovery("cleanup_pending"))).toBe(false);
    expect(recoveryMayBeInProgress(recovery("cleanup_pending"))).toBe(false);
  });
});
