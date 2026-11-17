# Fork installation and release isolation (Issue #1)

Agent Tool Chain Manager is an independently installed fork of
https://github.com/xingkongliang/skills-manager (MIT licensed).
The upstream installation and this fork are separate products.

## Identity and data paths

| Surface | Upstream | This fork |
| --- | --- | --- |
| App name | Skills Manager | Agent Tool Chain Manager |
| Tauri identifier | com.agentskills.desktop | com.wzwu0309.agenttoolchainmanager |
| Desktop executable | skills-manager | agent-tool-chain-manager |
| CLI executable | skills-manager-cli | agent-tool-chain-manager-cli |
| Central store and app-managed CLI bridge | ~/.skills-manager | ~/.agent-tool-chain-manager |
| Config directory below OS config directory | skills-manager | agent-tool-chain-manager |
| Git backup credential keyring service | skills-manager-git-backup | agent-tool-chain-manager-git-backup |

The fork is **clean-install only**. It must not automatically read, migrate,
overwrite, or delete the upstream installation's store, preferences, CLI
bridge, or credentials. If a user deliberately configures the fork's central
store to point at an upstream-managed directory, that overrides this isolation
and is unsupported unless a separate migration review has been performed.

The internal repository metadata protocol still uses .skills-manager/ and
some internal files still use the skills-manager name (for example
skills-manager.db inside the independent central store). This is intentional
to preserve format compatibility. Repository metadata paths and Git backup
references are not the upstream application's *default* home directory and
must not be renamed just for cosmetic branding.

## Updates and release security

Application update checking, download and installation are deliberately
disabled until this fork has an independently signed release channel.
The fork does not query or install the upstream's app releases.
Tauri updater runtime registration, permission and updater artifact creation
are all disabled. The Settings screen has a read-only link to the fork's
releases page. Skill-source updates are separate and remain functional.

The inherited GitHub release workflow is disabled in this repository by a
repository-name guard, and does not run automatically on a version tag.
Before enabling fork releases, configure fork-owned signing certificates,
Apple notarization credentials, signed updater metadata and validation,
Windows/Linux artifact verification, release permissions, and an install,
side-by-side and rollback test. Do not reuse upstream updater public keys or
upstream signing credentials.

## CLI compatibility and migration

New scripts and bundled manage-skills instructions use
agent-tool-chain-manager-cli. The old skills-manager-cli name is **not** an
alias in this fork, so a PATH installation cannot replace upstream's CLI.
Automation originally invoking the upstream CLI must opt into the new binary.

Existing upstream data remains in place. No migration/cleanup of upstream
data is performed by this patch, and this patch never clears original Agent
global directories. To roll back, exit/remove the fork application and keep
the upstream installation and its data as-is.

## Verification before closing this issue

1. Run npm run test:isolation for executable source/config guardrails.
2. Execute Rust unit tests and frontend test, lint and build on a host with
   Rust/Cargo and Node tooling.
3. Package on macOS Apple Silicon and inspect the actual bundle executable,
   CFBundleIdentifier, code signature and app-data/log locations.
4. Install the fork alongside upstream in a disposable OS account or VM.
   Verify independent store/config/keyring/CLI bridge paths and that no
   accidental writes to upstream paths occur.
5. Inspect app-start and Settings behavior for updater network requests and
   ensure the UI cannot download/install an upstream application release.

Do not mark installation/runtime verification complete without these tests.
