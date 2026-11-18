// No third-party dependencies: executable prebuild guardrails for Issue #1.
import assert from "node:assert/strict";
import { readFileSync, existsSync } from "node:fs";
import { test } from "node:test";

const read = (path) => readFileSync(path, "utf8");
const tauri = JSON.parse(read("src-tauri/tauri.conf.json"));
const capability = JSON.parse(read("src-tauri/capabilities/default.json"));
const pkg = JSON.parse(read("package.json"));
const lock = JSON.parse(read("package-lock.json"));

test("fork application identity is independent of upstream", () => {
  assert.equal(pkg.name, "agent-toolchain-manager");
  assert.equal(lock.name, pkg.name);
  assert.equal(lock.packages[""].name, pkg.name);
  assert.equal(tauri.productName, "Agent Toolchain Manager");
  assert.equal(tauri.identifier, "com.wzwu0309.agenttoolchainmanager");
  assert.notEqual(tauri.identifier, "com.agentskills.desktop");
  assert.equal(tauri.app.windows[0].title, "Agent Toolchain Manager");
  const manifest = read("src-tauri/Cargo.toml");
  assert.match(manifest, /name = "agent-toolchain-manager"/);
  assert.match(manifest, /default-run = "agent-toolchain-manager"/);
  assert.match(read("src-tauri/Cargo.lock"), /name = "agent-toolchain-manager"/);
});

test("fork retains GitHub and UI branding", () => {
  assert.match(read("index.html"), /<title>Agent Toolchain Manager<\/title>/);
  for (const p of ["README.md", "README.zh-CN.md"]) {
    assert.match(read(p), /<h1 align="center">Agent Toolchain Manager<\/h1>/);
    assert.match(read(p), /wzwu0309\/agent-toolchain-manager/);
  }
});

test("manage-skills can use PR ref during pre-merge verification", () => {
  const ui = read("src/components/AgentControlSetupCard.tsx");
  assert.match(ui, /VITE_MANAGE_SKILLS_REF/);
  assert.match(ui, /\|\| "main"/);
  assert.match(ui, /wzwu0309\/agent-toolchain-manager\/tree/);
  assert.match(ui, /skills\/manage-skills/);
});

test("fork store, config, keyring and CLI bridge do not reuse upstream identities", () => {
  const central = read("src-tauri/src/core/central_repo.rs");
  assert.match(central, /return path.join\("\.agent-toolchain-manager"\)/);
  assert.match(central, /\.join\("\.agent-toolchain-manager"\)/);
  assert.match(central, /\.join\("agent-toolchain-manager"\)/);
  assert.doesNotMatch(central.split("mod tests {")[0], /\.join\("\.skills-manager"\)/);
  assert.doesNotMatch(central.split("mod tests {")[0], /\.agent-skills/);
  assert.match(read("src-tauri/src/core/git_credentials.rs"), /KEYRING_SERVICE: &str = "agent-toolchain-manager-git-backup"/);
  const bridge = read("src-tauri/src/core/cli_bridge.rs");
  assert.match(bridge, /"agent-toolchain-manager-cli"/);
  assert.doesNotMatch(bridge, /"skills-manager-cli"/);
  assert.ok(existsSync("src-tauri/src/bin/agent-toolchain-manager-cli.rs"));
  assert.ok(!existsSync("src-tauri/src/bin/skills-manager-cli.rs"));
  assert.match(read("scripts/run-rust-cli.mjs"), /agent-toolchain-manager-cli/);
  assert.match(read("skills/manage-skills/SKILL.md"), /\.agent-toolchain-manager\/bin/);
});

test("upstream application update installation is structurally disabled", () => {
  assert.equal(tauri.bundle.createUpdaterArtifacts, false);
  assert.ok(!tauri.plugins?.updater);
  assert.ok(!capability.permissions.some((p) => p.startsWith("updater:")));
  assert.doesNotMatch(read("src-tauri/src/lib.rs"), /\.plugin\(tauri_plugin_updater::/);
  const settings = read("src-tauri/src/commands/settings.rs");
  assert.match(settings, /updates_enabled: false/);
  assert.match(settings, /has_update: false/);
  assert.match(settings, /wzwu0309\/agent-toolchain-manager\/releases/);
  assert.doesNotMatch(settings, /api.github.com\/repos\/xingkongliang\/skills-manager\/releases/);
  const frontend = read("src/views/Settings.tsx");
  assert.doesNotMatch(frontend, /checkUpdater\(|downloadAndInstall\(/);
  assert.match(frontend, /forkUpdatesDisabled/);
  assert.match(frontend, /wzwu0309\/agent-toolchain-manager\/releases/);
});

test("inherited upstream release workflow is disabled in the fork", () => {
  const workflow = read(".github/workflows/release.yml");
  assert.doesNotMatch(workflow, /\n  push:\s*\n    tags:/);
  const guards = workflow.match(/if: github.repository == 'xingkongliang\/skills-manager'/g) || [];
  assert.equal(guards.length, 3);
  assert.match(read(".github/workflows/prepare-release.yml"), /if: github.repository == 'xingkongliang\/skills-manager'/);
});

test("fork update copy and app branding exist in each locale", () => {
  for (const lang of ["en", "zh", "zh-TW", "es", "ko"]) {
    const translation = JSON.parse(read("src/i18n/" + lang + ".json"));
    assert.equal(translation.app.name, "Agent Toolchain Manager");
    assert.ok(translation.settings.forkUpdatesDisabled);
    assert.ok(translation.settings.forkReleases);
  }
});
