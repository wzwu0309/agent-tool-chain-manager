// @vitest-environment jsdom
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { MemoryRouter } from "react-router-dom";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { ProjectSkillRecovery } from "../lib/tauri";

const providers = vi.hoisted(() => ({
  active: vi.fn(),
  orphan: vi.fn(),
  onInspection: null as null | ((event: { payload: { ok: boolean } }) => void),
}));
vi.mock("../lib/tauri", () => ({
  getActiveProjectSkillRecoveries: providers.active,
  getUnregisteredProjectSkillRecoveries: providers.orphan,
}));
vi.mock("../context/AppContext", () => ({
  useApp: () => ({
    tools: [],
    projects: [{ id: "project-1", name: "My Project", sync_health: { diverged: 0 } }],
    managedSkills: [],
    openSkillDetailById: vi.fn(),
  }),
}));
vi.mock("../components/AgentControlSetupCard", () => ({ AgentControlSetupCard: () => null }));
vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn(async (_name: string, handler: (event: { payload: { ok: boolean } }) => void) => {
    providers.onInspection = handler;
    return () => { providers.onInspection = null; };
  }),
}));
vi.mock("react-i18next", () => ({
  useTranslation: () => ({
    t: (key: string) => key,
  }),
}));

import { Dashboard } from "./Dashboard";

const sample = (phase: string): ProjectSkillRecovery => ({
  operation_id: "op-1",
  project_id: "project-1",
  skill_id: "skill-1",
  deployment_id: "deployment-1",
  kind: "update",
  phase,
  target_path: "/my-project/.claude/skills/demo",
  recovery_root: "/my-project/.claude/skills/.atm-recovery/op-1",
  destination_path: null,
  target_exists: false,
  recovery_root_exists: true,
  retained_paths: ["/my-project/.claude/skills/.atm-recovery/op-1/previous"],
  created_at: 1,
  updated_at: 2,
});

let host: HTMLElement;
let root: Root;
beforeEach(() => {
  host = document.createElement("div");
  document.body.appendChild(host);
  root = createRoot(host);
  providers.active.mockReset();
  providers.orphan.mockReset();
  providers.orphan.mockResolvedValue([]);
  providers.onInspection = null;
  (globalThis as unknown as { IS_REACT_ACT_ENVIRONMENT: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
});
afterEach(async () => {
  await act(async () => { root.unmount(); });
  host.remove();
});

async function renderInbox() {
  await act(async () => {
    root.render(<MemoryRouter><Dashboard /></MemoryRouter>);
  });
}

describe("active-project recovery inbox", () => {
  it("displays an actionable, read-only interrupted recovery and the actual backup location", async () => {
    providers.active.mockResolvedValue([sample("needs_attention")]);
    await renderInbox();
    expect(host.textContent).toContain("dashboard.recoveryInbox.title");
    expect(host.textContent).toContain("My Project");
    expect(host.textContent).toContain("dashboard.recoveryInbox.mayBeUnavailable");
    expect(host.textContent).toContain("/my-project/.claude/skills/.atm-recovery/op-1/previous");
    expect(host.querySelector('button')).not.toBeNull();
  });

  it("does not report a normal retained cleanup_pending backup as an outage", async () => {
    providers.active.mockResolvedValue([sample("cleanup_pending")]);
    await renderInbox();
    expect(host.textContent).not.toContain("dashboard.recoveryInbox.title");
    expect(host.querySelector('[role="alert"]')).toBeNull();
  });

  it("re-queries the inbox after background recovery inspection finishes", async () => {
    providers.active.mockResolvedValue([sample("cleanup_pending")]);
    await renderInbox();
    expect(host.textContent).not.toContain("dashboard.recoveryInbox.title");
    providers.active.mockResolvedValue([sample("needs_attention")]);
    await act(async () => {
      providers.onInspection?.({ payload: { ok: true } });
    });
    expect(host.textContent).toContain("dashboard.recoveryInbox.title");
    expect(host.textContent).toContain("My Project");
  });

  it("surfaces an incomplete startup scan instead of claiming the inbox is clear", async () => {
    providers.active.mockResolvedValue([]);
    await renderInbox();
    await act(async () => {
      providers.onInspection?.({ payload: { ok: false } });
    });
    expect(host.textContent).toContain("dashboard.recoveryInbox.statusUncertain");
  });

  it("shows unknown status when the SQLite inbox cannot be queried", async () => {
    providers.active.mockRejectedValue(new Error("injected SQLite read failure"));
    await renderInbox();
    expect(host.textContent).toContain("dashboard.recoveryInbox.statusUncertain");
  });
});
