<p align="center">
  <img src="assets/icon.png" width="80" alt="Agent Toolchain Manager" />
</p>

<h1 align="center">Agent Toolchain Manager</h1>

<p align="center">
  A project-first home for reusable AI agent capabilities.
</p>

<p align="center">
  <a href="./README.zh-CN.md">简体中文</a>
  &nbsp;·&nbsp;
  <a href="https://github.com/wzwu0309/agent-toolchain-manager/issues">Issues &amp; development</a>
</p>

## Vision

AI coding agents should be able to use the right capabilities in the right project without every tool requiring a separate, manually maintained setup.

**Agent Toolchain Manager** aims to become a shared control plane for organizing, assigning and maintaining reusable agent capabilities across tools and projects. The intended workflow is:

**Discover → Curate → Choose agents/projects → Review changes → Deploy → Track and update.**

The goal is not to make every agent identical. It is to make each project's setup explicit, repeatable, inspectable and safe to change.

## What works today

The current application focuses on **Skills**:

- **Central Skill library:** import from Git repositories, local directories, archives and the [skills.sh](https://skills.sh) marketplace. The fork defaults to `~/.agent-toolchain-manager` and supports a custom library path.
- **Agent and project workspaces:** inspect global and project-local Skills, manage supported agents, and deploy by copy or symlink.
- **Presets and organization:** assign groups of Skills, use tags and filters, and manage multiple deployments.
- **GUI and CLI:** people can work in the desktop app; coding agents can use the `agent-toolchain-manager-cli` and the bundled [`manage-skills`](skills/manage-skills/SKILL.md) Skill.
- **History and backup:** use Git-based backup, snapshots and multi-device synchronization, with conflict-handling controls.
- **Inspection:** view Skill documentation, sources and deployment state, and export application logs.

These are the current Skill-management capabilities; they do **not** mean generalized MCP/Plugin/command management is implemented.

## Where we want to go

These are **directions for future development, not shipped features or delivery promises**:

1. **Project-first configuration:** select the agents and capabilities a particular repository needs, without changing unrelated projects or global agent folders.
2. **Traceable versions and safer updates:** make deployed content, provenance, local edits and explicit takeover decisions visible before replacing files.
3. **Broader capability assets:** explore managing MCP servers, commands, prompts and other reusable agent integrations alongside Skills, once their ownership and lifecycle contracts are clear.
4. **Consistent automation:** have the GUI and agent-facing CLI use the same predictable validation, conflict and deployment rules.
5. **Reproducible toolchains:** make it easier to understand and recreate a project's agent setup on another machine.

Implementation priorities and design decisions are tracked in [GitHub Issues](https://github.com/wzwu0309/agent-toolchain-manager/issues).

## Install this fork from source

No independently signed installer is available for this fork yet. Upstream
Homebrew, Scoop and GitHub Releases commands install the original Skills
Manager, **not** Agent Toolchain Manager.

On a machine with Rust, Cargo, Node and Tauri build prerequisites:

    npm ci
    npm run tauri:build

See [Fork release isolation](FORK_RELEASE_ISOLATION.md) before installation.

## Quick Start

1. Install skills from local folders, Git repositories, archives, or the marketplace.
2. Open **Global Workspace** from the sidebar and pick an agent (e.g. Claude Code).
3. Click a **Preset** pill to activate its skills for that agent, or use **+ Add Skills** to pick from your library and toggle target agents inline. Active presets show a ✓; partial installs show a count badge.
4. To manage project-local skills, open a **Project Workspace** and use the same preset pills or the **+ Add Skills** picker with its multi-agent target selector.
5. Configure agent paths, custom tools, theme, language, proxy, and Git preferences in **Settings**.
6. If you want history or multi-machine sync, open **Backup** in the sidebar and click **Sign in with GitHub** — backup and cross-device sync run automatically from then on.

## Let your agents manage skills

Claude Code, Codex, Cursor and the rest can install a skill, deploy it to another agent, or report what is where — by driving Agent Toolchain Manager rather than writing into an agent's folder behind its back. That is what keeps source metadata, preset membership, update tracking and cross-agent deployment state intact.

The Dashboard offers a one-time setup: pick the agents that should be able to do it, and the app installs the [`manage-skills`](skills/manage-skills/SKILL.md) skill and deploys it to exactly those. Afterwards it is an ordinary library skill — adding or removing an agent is the agent badge row on its own card. No PATH setup is involved: the app publishes a copy of its CLI where agents look for it.

It is also an ordinary published skill, so it can be installed without the app:

```bash
npx skills add wzwu0309/agent-toolchain-manager
```

## Backup & Multi-Device Sync

The **Backup** page (sidebar) keeps your skill library versioned in a Git repository. One device gets versioned backup with restorable snapshots; several devices connected to the same repository stay in sync with each other automatically. The remote stays a plain Git repository — you can `git clone` it anywhere, no lock-in.

### Connect

- **Sign in with GitHub** (recommended): an 8-digit device-flow sign-in creates a private `skills-manager-backup` repository for you. The token is stored in the OS keychain — never in files or the repo config.
- **Advanced**: paste any Git URL (HTTPS + PAT, SSH, self-hosted) under **Settings → Git Sync Configuration**.
- On a new machine with an empty library, the first launch asks: **start fresh, or restore from a backup?**

### How syncing works

- **Automatic**: local changes are committed and pushed in the background a couple of minutes after you stop editing; updates pushed by your other devices are merged in and pushed back automatically. **Back Up Now** is always available for an immediate run, and every backup in the history shows which device made it.
- **Skill-aware merging**: changes are merged per skill, not per text line — renaming a skill on one machine combines cleanly with editing its content on another.
- **Conflicts never block or overwrite**: if the same skill was edited on two devices at once, everything else syncs normally while that skill keeps your local version and appears under **Needs attention** (also badged on its card in the Library). Pick **keep mine / use remote / keep both** — a safety snapshot is taken before any choice is applied, so every decision is undoable.
- **Snapshots & restore**: manual backups create snapshot versions; open the Backup page history to restore any of them. A restore first saves the current state as its own snapshot.

### What's included

Skills, tags, presets, and per-agent skill toggles are backed up. Secrets (API keys, tokens, proxy settings) and machine-specific wiring never leave the machine. Skills over 100 MB stay local and are excluded from backup automatically (labeled on the Backup page). The SQLite database is not in Git — it stores metadata that is rebuilt from the skill files.

### Disconnecting

The Backup page offers three levels: **disconnect this machine** (other devices and remote data untouched), **revoke the GitHub authorization**, or **delete the remote backup** entirely (routed through GitHub's own type-the-name confirmation).

## Supported Tools

56 agents are supported out of the box, including:

Claude Code · Codex · Cursor · GitHub Copilot · Gemini CLI · GitLab Duo · OpenCode · OpenClaw · Hermes Agent · OpenHands · Cline · Goose · Windsurf · Continue · Grok · Antigravity · Qwen Code · ZCode · Crush · Kilo Code · Roo Code · Amp · Kiro CLI · Droid · TRAE IDE · Warp · Qoder · CodeBuddy · MiniMax Code

**Settings** lists them all, leading with the ones detected on your machine. You can also add custom tools there and manage their skills the same way.

## Tech Stack

| Layer | Tech |
|-------|------|
| Frontend | React 19, TypeScript, Vite, Tailwind CSS |
| Desktop | Tauri 2 |
| Backend | Rust |
| Storage | SQLite (`rusqlite`) |
| i18n | react-i18next |

## Getting Started

### Prerequisites

- Node.js 20.19+ or 22.12+ (required by Vite 7)
- Rust 1.77.2 or newer
- [Tauri prerequisites](https://v2.tauri.app/start/prerequisites/) for your OS

### Development

```bash
npm install
npm run tauri:dev
```

### CLI

The repository includes an agent-friendly CLI built on the same Rust shared core used by the desktop app. Both the CLI and the desktop app go through the same SQLite database, central library, and sync engine.

```bash
# Look around
npm run cli -- skills list
npm run cli -- skills show db

# Install into the library (does NOT deploy to any agent by itself)
npm run cli -- skills install ./my-skill
npm run cli -- skills install https://github.com/foo/bar/tree/main/skills/baz
npm run cli -- skills install vercel-labs/agent-skills@react-best-practices
npm run cli -- skills install https://www.skills.sh/site/uizze.com/ui-radar # website-synced skill

# Put it into the agents that should have it, then check
npm run cli -- skills deploy react-best-practices --agent claude_code --agent codex
npm run cli -- skills status react-best-practices

# Pull upstream changes, and adopt what an agent already has
npm run cli -- skills check --all
npm run cli -- skills update --all
npm run cli -- skills adopt ~/.claude/skills --dry-run
```

`--help` on any group or subcommand prints the full surface — the groups below
each carry more than these examples show. `--dry-run` is available on selected
commands, including `skills deploy/undeploy/sync/remove/adopt` and
`presets deploy/undeploy/delete`; check each subcommand's help before using it.
`skills remove` requires `--yes` for actual deletion.

Available command groups:
- `repo` — inspect or change the configured base directory
- `agents` (`tools` alias) — list agents, globally enable or disable them, and add custom agents (`add-custom`)
- `skills` — manage the central library and real per-agent deployments (`deploy / undeploy / status`)
- `presets` — create, update, delete, organize, deploy, undeploy, and inspect presets
- `git` — operate on the git-backed `skills/` repository (`clone`, `pull`, `push`, `commit`, `versions`, `restore`)

Extra flags:
- `--skills-root <path>` — operate on a cloned/exported skills repo directly instead of the local app default. The manager's state (DB, presets, cache, logs) lives in `~/.agent-toolchain-manager/external/<name>-<hash>/`, namespaced by the canonical path of the skills root, so the external checkout itself stays clean.
- `--json` — machine-readable output for scripts/agents. Failures print `{"ok": false, "code": …, "message": …}` on stderr with a non-zero exit. A deployment refused because the target is not ours carries the paths as data (`code: "TARGET_CONFLICT"`, `details.conflicts[].path`) so a caller can name the directory in the way instead of quoting a sentence.

```bash
npm run -s cli -- --skills-root /path/to/my-skills --json skills list
```

#### Agents inside WSL

The Windows app cannot create links inside WSL, so it can only copy skills there. If your agents live in WSL, run the Linux CLI inside WSL instead — it keeps its own library there and deploys with real symlinks:

```bash
# Build the CLI from this fork on Linux, with Rust/Cargo and Node:
npm run cli:build
# Place src-tauri/target/debug/agent-toolchain-manager-cli on PATH.
agent-toolchain-manager-cli agents add-custom hermes-work --path ~/.hermes/profiles/work/skills
agent-toolchain-manager-cli skills install ./my-skill
agent-toolchain-manager-cli skills deploy my-skill --agent hermes-work
```

#### Where the binary lives

At startup the app publishes a copy of its own CLI to `~/.agent-toolchain-manager/bin/agent-toolchain-manager-cli`, always matching the running app, so agents can find it without anything on your PATH. A `.version` stamp beside it is written only after the copy is verified and removed before each republish, so a copy that failed — a binary held open on Windows, say — is never presented as usable.

Putting the CLI on your *own* PATH, for typing commands yourself, is separate:

```bash
npm run cli:install
# equivalent to:
# cargo install --path src-tauri --bin agent-toolchain-manager-cli --locked --force
```

This drops the binary at `~/.cargo/bin/agent-toolchain-manager-cli`. Re-run after pulling updates to refresh it.

This fork currently has no signed standalone CLI releases. When available, use only this fork's `agent-toolchain-manager-cli-*` asset, make it executable on macOS/Linux, and place it on PATH.

#### Concurrent use with the desktop app

The CLI and desktop app share the same SQLite database and repository lock. The app's filesystem watcher normally refreshes after CLI metadata or deployment changes. If the app was suspended while a command ran, trigger one manual refresh.

### Build

```bash
npm run tauri:build
npm run cli:build
```

## Troubleshooting

**macOS cannot open a locally built app.** This fork does not yet publish an independently signed and notarized installer. Verify that you built or obtained the app from a trusted source, and consult macOS **System Settings → Privacy & Security** for the applicable warning. Signing and notarization claims about the upstream application's historical releases do not apply to this fork.

For other issues, [open an issue](https://github.com/wzwu0309/agent-toolchain-manager/issues) and attach logs exported from **Settings → Export Logs**.

## Origins and license

Agent Toolchain Manager is an independent fork of [Skills Manager](https://github.com/xingkongliang/skills-manager), originally released under the **MIT license**. We credit the upstream project for its original implementation; its promotional videos, popularity metrics and release history are not this project's claims.

This repository is licensed under MIT. See [LICENSE](LICENSE).
