# Fork installation and release isolation (Issue #1)

Agent Toolchain Manager is an independently installed fork of
https://github.com/xingkongliang/skills-manager (MIT licensed).
The upstream installation and this fork are separate products.

## Identity and data paths

| Surface | Upstream | This fork |
| --- | --- | --- |
| App name | Skills Manager | Agent Toolchain Manager |
| Tauri identifier | com.agentskills.desktop | com.wzwu0309.agenttoolchainmanager |
| Desktop executable | skills-manager | agent-toolchain-manager |
| CLI executable | skills-manager-cli | agent-toolchain-manager-cli |
| Central store and app-managed CLI bridge | ~/.skills-manager | ~/.agent-toolchain-manager |
| Config directory below OS config directory | skills-manager | agent-toolchain-manager |
| Git backup credential keyring service | skills-manager-git-backup | agent-toolchain-manager-git-backup |

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

## Naming migration from the previous fork (Issue #8)

The previous development fork was called **Agent Tool Chain Manager**, with
the default library at `~/.agent-tool-chain-manager`, a CLI called
`agent-tool-chain-manager-cli`, and OS configuration under
`agent-tool-chain-manager/`. The renamed fork uses **Agent Toolchain Manager**,
`~/.agent-toolchain-manager`, and `agent-toolchain-manager-cli`.

The **Tauri bundle identifier is unchanged**:
`com.wzwu0309.agenttoolchainmanager`. This is a naming migration within the
fork, not a new installation of upstream Skills Manager.

Do **not** run the old application and new one at the same time. The app will
refuse to initialize a *new, empty* default library when it detects the previous
fork's directory or configuration without the new paths. It does **not**
silently copy, rename, remove or overwrite previous data.

For an existing default-path installation:

1. Quit the old App and any CLI process using the library. Back up the original
   directory, including `skills-manager.db`, its WAL and SHM, and `.secret.key`.
2. Verify `~/.agent-toolchain-manager` does **not** already exist; if it does,
   investigate it before changing anything.
3. On the same disk, rename the **whole** directory, not just the database:

   ```sh
   if [ -e "$HOME/.agent-toolchain-manager" ]; then
     echo "Target already exists; inspect it before proceeding"
   elif [ ! -d "$HOME/.agent-tool-chain-manager" ]; then
     echo "Previous store not found; do not move anything"
   else
     mv "$HOME/.agent-tool-chain-manager" "$HOME/.agent-toolchain-manager"
     sqlite3 "$HOME/.agent-toolchain-manager/skills-manager.db" 'PRAGMA quick_check;'
   fi
   ```

   Do not execute the move while either app is open.

4. Build and run the renamed app. It republishes the **new** CLI bridge
   (`bin/agent-toolchain-manager-cli`). The old binary may remain in `bin/`;
   **do not run it against the renamed store**. Review old agent instructions
   or PATH entries that still reference the predecessor name.
5. Review any project/symlink deployments or explicitly customized library
   location. Absolute paths stored in a database or symlink may require
   repointing through the existing library migration logic. A previous
   `repo-config.json` with a custom library location needs separate review and
   careful migration into the new OS config namespace.
6. The keyring service has a new name. Existing Git backup credentials in
   the old service are **not** copied or deleted automatically; re-enter backup
   remote credentials through the renamed app if necessary.

To roll back, quit the renamed app before restoring a verified backup or
renaming the library back. Do not run the old app against the new data while
its schema or contents may have evolved.

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
agent-toolchain-manager-cli. The old skills-manager-cli name is **not** an
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
