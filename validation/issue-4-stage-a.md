# Issue #4 — 阶段 A 独立基线验收（2026-11-18）

## 范围与基线

- GitHub: [Issue #4](https://github.com/wzwu0309/agent-toolchain-manager/issues/4)
- 范围：**SM-01、SM-02、SM-03、SM-04、SM-05、SM-10**；按照 Issue #4 的 2026-11-18 新契约，SM-06～SM-09 留待 [Issue #11](https://github.com/wzwu0309/agent-toolchain-manager/issues/11) 的对应里程碑之后独立验收。
- 被测源码：`origin/main` 的 **`0c8ccbabf251e1ad767fef2c2886b53f4fda04ac`**（已包含 PR #13 的项目注册延迟创建目录）。此项记录的是**源代码基线**，不是报告提交后的测试脚本所在提交。
- 执行环境：**macOS / Apple Silicon ARM64**；Rust/Cargo **1.100.0**，Node **v24.21.0**，npm **11.19.0**。
- DSH 工作区仍在仓库根目录，但**所有编译、CLI 测试、测试数据写入、证据收集均在** `.worktrees/issue-4` 独立 Git worktree 执行；开发分支 `test/issue-4-baseline-validation`。未在主工作树及其他 Agent 使用的 worktree 上运行测试或修改文件。
- 单元测试和 CLI 实际行为分开评估，**不能互相替代**。

## 方法与隔离边界

从 Issue #4 worktree 根目录运行：

```sh
cargo build --locked --manifest-path src-tauri/Cargo.toml --bin agent-toolchain-manager-cli
node scripts/validation/issue-4-stage-a.mjs
cargo test --locked --manifest-path src-tauri/Cargo.toml --lib commands::projects::tests -- --test-threads=4
cargo test --locked --manifest-path src-tauri/Cargo.toml --bin agent-toolchain-manager-cli project -- --test-threads=4
```

脚本会拒绝在**主工作树**或非 Issue #4 分支运行，并且只使用当前 worktree 内编译的 CLI；测试夹具放入被 `.gitignore` 忽略的 `.npm-cache/issue4-stage-a-*`，默认在运行结束后删除。`--keep` 可保留夹具用于人工检查。

子进程使用独立 `HOME`、`XDG_CONFIG_HOME`、`XDG_DATA_HOME`、`XDG_CACHE_HOME` 和 `TMPDIR`；临时 HOME 中先建立模拟 `.codex/skills`、`.claude/skills`、`.agents/skills` 用户级目录，放置带 SHA-256 的哨兵文件，测试前后逐项对比。项目 A/B 和源 Skill 均为新建测试夹具，**不指向或引用真实 Agent 用户目录/真实业务项目**。

CLI `repo status` 实测中央库、技能路径、SQLite DB 均在临时 HOME 下；如不满足该路径约束，脚本会在导入/部署前中止后续验证。命令与回执、文件树、SHA-256、符号链接解析均记录于同目录的 [`issue-4-stage-a-evidence.json`](./issue-4-stage-a-evidence.json)。公开日志中的绝对夹具路径已用 `<fixture>` 替换，无用户真实 HOME 字符串。

## 端到端结果

| Case | 结果 | 证据 / 重要限制 |
|---|---|---|
| **SM-01** | **PASS** | `skills install --local` → `synced=false`；`central_path=<fixture>/home/.agent-toolchain-manager/skills/issue4-fixture`；未在项目 A/B 或模拟用户级 Agent 目录下部署。初始 Skill 文本 SHA-256 `cf677759942c2662d244c98056e1fa4fa9a172d6e9f8f35f65574b0adbb18a4c`。 |
| **SM-02** | **PASS** | `projects add` 不创建 Agent 专属目录；`projects deploy --dry-run` 不写入；项目 A 实际部署 Codex、Claude Code 两个目标（`changed_pairs=2`），项目 B 的文件树与事前快照完全一致。 |
| **SM-03** | **FAIL（Codex）** | **以官方文档为验收依据，无须运行 Agent CLI**。OpenAI Codex 文档要求项目级 `$REPO_ROOT/.agents/skills/<skill>/SKILL.md`，实际管理器部署在 `.codex/skills/<skill>/SKILL.md`，**不符合文档**；Claude Code 文档的 `.claude/skills/<skill>/SKILL.md` 与实测一致。两条符号链接都存在，`projects show` 识别到两个目标；不能因此推断 Codex 按官方标准可发现该 Skill。参见下方文档依据。 |
| **SM-04** | **PASS** | 两个项目级部署目标均为符号链接，`realpath` 均指向同一中央 Skill（`<fixture>/home/.agent-toolchain-manager/skills/issue4-fixture`）。 |
| **SM-05** | **PARTIAL** | 在项目 A 的 Codex 链接中追加临时文本，中央 Skill SHA-256 从 `cf6777…b18a4c` 变为 `0010a0…d2cbaa0`，Claude 链接读取相同变更；之后恢复中央原文件并校验原 Hash。**共享写入风险已实测**，但 GUI 提示未通过真实 UI 验证。 |
| **SM-10** | **PASS** | 项目部署运行前后的临时用户级 `.codex`、`.claude`、`.agents` 全目录 SHA-256/路径清单完全一致；项目 B 没有产生 Agent 目录。此结论限定在隔离 CLI 流程，不推广为整个桌面 App 全行为无副作用。 |
| **SM-06～09** | **NOT TESTED** | 根据 Issue #4 最新分阶段契约：等待 Issue #11 的 Commit/Tree、安全 Copy、受管所有权、GUI/CLI 统一契约落地，不用旧 Copy 语义凑数。 |

**阶段 A 汇总：4 PASS、1 PARTIAL、1 FAIL；阶段 B 4 NOT TESTED。** 验证脚本在存在 FAIL 时按约定返回非零退出码（当前为 1），这是预期的准确失败信号，不是脚本崩溃。 FAIL 是 Codex 项目级部署路径不符合官方文档，并非声称已用 Codex 子进程复现 Skill 无法被发现。

## SM-03：官方文档对照（2026-11-18）

用户确认：**使用各 Agent 的官方 Skills 文档对项目路径进行验收，不需要运行 Agent CLI 自身验证**。按这一口径更新本轮阶段 A；`projects show` 仅作为管理器内部的补充证据。

| 产品 | 官方来源 | 官方项目级路径 | 实际 CLI 部署路径 | 结论 |
|---|---|---|---|---|
| Codex | [OpenAI 官方 Build skills / Where Codex loads local skills](https://learn.chatgpt.com/docs/build-skills#where-to-save-skills)；[OpenAI CLI 安装文档中的 Agent 路径表](https://developers.openai.com/api/reference/cli#use-the-cli-from-a-coding-agent) | `.agents/skills/<skill>/SKILL.md`（Codex 扫描当前工作目录及仓库根目录范围内的 `.agents/skills`，支持 Skill 目录的符号链接） | `.codex/skills/issue4-fixture/SKILL.md` | **FAIL：文档路径不一致** |
| Claude Code | [Anthropic 官方 Extend Claude with skills / Choose where skills load](https://code.claude.com/docs/en/skills#choose-where-skills-load) | `.claude/skills/<skill>/SKILL.md`，支持目录符号链接 | `.claude/skills/issue4-fixture/SKILL.md` | **PASS：与文档一致** |

仓库 [`src-tauri/src/core/tool_adapters.rs`](../src-tauri/src/core/tool_adapters.rs) 中 `codex` 的 `relative_skills_dir` 仍设置为 `.codex/skills`；临近注释也需要与当前官方文档对齐。**本验证分支不直接修改产品适配器**，以免与其他 Agent 在 #11 的并行实现交叉。路径修正已单列 [Issue #15](https://github.com/wzwu0309/agent-toolchain-manager/issues/15)，需独立评审，并核对可能共享 `.agents/skills` 的其他适配器的目标所有权与重名冲突。

这是一项**文档规范与实际部署的差异结论**，不额外声称当前某个 Codex 版本一定不会读取 `.codex/skills`，也不承诺云端/桌面端所有形态均采用完全相同的本地扫描规则。

## 单元回归（仅补充，非 E2E）

- `cargo test --lib commands::projects::tests`：**19 passed / 0 failed**（666 filtered out）。
- `cargo test --bin agent-toolchain-manager-cli project`：**10 passed / 0 failed**（7 filtered out）。
- 两条命令均在 Issue #4 worktree 下运行，Rust 编译结果位于该 worktree 自己的 `src-tauri/target/`，没有与其他 Agent 共享写入构建目录。

## 额外发现 / 后续建议

1. **SM-03 的结论与修复**：以当前官方文档判定 Codex 路径不符合项目级发现标准（[Issue #15](https://github.com/wzwu0309/agent-toolchain-manager/issues/15)）；Claude Code 路径符合。无需启动实际 Agent CLI；应独立修复 Codex 适配器的项目路径、相关注释与共享目录冲突语义，之后再按官方文档和实际部署树复验。
2. **SM-05 的 UI 风险说明**：当前 [中文设置文案](../src/i18n/zh.json) 的 `settings.syncModeDesc` 提及“符号链接……修改可实时生效”及“链接不会随仓库带走”，但没有直接表述 **修改项目中的链接 Skill 会修改中央 Store，并同时影响其他项目**。应在 UI 交互验证时检查用户能否在做出共享写入风险操作前获得足够明确的提示；必要时单列小型 UX Issue。
3. 阶段 B 的测试设计应基于 [#11](https://github.com/wzwu0309/agent-toolchain-manager/issues/11) 的 **选定 Commit / Skill Tree / DeploymentId / 授权接管**，覆盖双方分叉、同 mtime、未授权目标、GUI/CLI 关键语义。不得把 PR #9 或旧 Copy mtime 判定当作最终标准。
4. 本次没有启动真实桌面 GUI，不能宣称完成 SM-05 的 UI 提示或 SM-09 的 GUI/CLI 一致性验收。SM-03 采用官方文档对照，无需启动 Agent CLI。

## 交付物与后续状态

- 自动化脚本：[`scripts/validation/issue-4-stage-a.mjs`](../scripts/validation/issue-4-stage-a.mjs)
- 真实运行证据：[`validation/issue-4-stage-a-evidence.json`](./issue-4-stage-a-evidence.json)，包含实际 CLI argv/回执、目录清单、文件 Hash、环境与各项状态。
- 该报告可以作为阶段 A 的审查依据，但 **Issue #4 应继续 Open**，待 SM-03 路径差异修复、SM-05 UI 提示补证以及阶段 B 在 #11 目标架构下完成。
