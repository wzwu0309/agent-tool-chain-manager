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
| **SM-03** | **PARTIAL** | 实测目标为项目 A 的 `.codex/skills/issue4-fixture` 和 `.claude/skills/issue4-fixture`，`projects show` 识别到两个 Skill 目标。但执行环境中 `codex` / `claude` 命令不可用，没有启动真实 Agent 验证其自身的 Skill 发现，因此**不能判定全部 PASS**。 |
| **SM-04** | **PASS** | 两个项目级部署目标均为符号链接，`realpath` 均指向同一中央 Skill（`<fixture>/home/.agent-toolchain-manager/skills/issue4-fixture`）。 |
| **SM-05** | **PARTIAL** | 在项目 A 的 Codex 链接中追加临时文本，中央 Skill SHA-256 从 `cf6777…b18a4c` 变为 `0010a0…d2cbaa0`，Claude 链接读取相同变更；之后恢复中央原文件并校验原 Hash。**共享写入风险已实测**，但 GUI 提示未通过真实 UI 验证。 |
| **SM-10** | **PASS** | 项目部署运行前后的临时用户级 `.codex`、`.claude`、`.agents` 全目录 SHA-256/路径清单完全一致；项目 B 没有产生 Agent 目录。此结论限定在隔离 CLI 流程，不推广为整个桌面 App 全行为无副作用。 |
| **SM-06～09** | **NOT TESTED** | 根据 Issue #4 最新分阶段契约：等待 Issue #11 的 Commit/Tree、安全 Copy、受管所有权、GUI/CLI 统一契约落地，不用旧 Copy 语义凑数。 |

**阶段 A 汇总：4 PASS、2 PARTIAL、0 FAIL；阶段 B 4 NOT TESTED。** 此处 PARTIAL 的含义是当前范围内已取得部分证据，但尚不足以证明原始验收条件完全满足。

## 单元回归（仅补充，非 E2E）

- `cargo test --lib commands::projects::tests`：**19 passed / 0 failed**（666 filtered out）。
- `cargo test --bin agent-toolchain-manager-cli project`：**10 passed / 0 failed**（7 filtered out）。
- 两条命令均在 Issue #4 worktree 下运行，Rust 编译结果位于该 worktree 自己的 `src-tauri/target/`，没有与其他 Agent 共享写入构建目录。

## 额外发现 / 后续建议

1. **SM-03 的剩余证据**：在受控环境中使用真实 Codex 和 Claude Code 客户端加载两个原生项目目录，记录产品实际列出的 Skill；仅靠工具管理器的 `projects show` 不够。当前 shell 的 PATH 中找不到这两个 Agent 的可执行文件，不代表用户未安装它们。
2. **SM-05 的 UI 风险说明**：当前 [中文设置文案](../src/i18n/zh.json) 的 `settings.syncModeDesc` 提及“符号链接……修改可实时生效”及“链接不会随仓库带走”，但没有直接表述 **修改项目中的链接 Skill 会修改中央 Store，并同时影响其他项目**。应在 UI 交互验证时检查用户能否在做出共享写入风险操作前获得足够明确的提示；必要时单列小型 UX Issue。
3. 阶段 B 的测试设计应基于 [#11](https://github.com/wzwu0309/agent-toolchain-manager/issues/11) 的 **选定 Commit / Skill Tree / DeploymentId / 授权接管**，覆盖双方分叉、同 mtime、未授权目标、GUI/CLI 关键语义。不得把 PR #9 或旧 Copy mtime 判定当作最终标准。
4. 本次没有启动真实桌面 GUI，不能宣称完成 SM-03 的 Agent 发现、SM-05 的 UI 提示或 SM-09 的 GUI/CLI 一致性验收。

## 交付物与后续状态

- 自动化脚本：[`scripts/validation/issue-4-stage-a.mjs`](../scripts/validation/issue-4-stage-a.mjs)
- 真实运行证据：[`validation/issue-4-stage-a-evidence.json`](./issue-4-stage-a-evidence.json)，包含实际 CLI argv/回执、目录清单、文件 Hash、环境与各项状态。
- 该报告可以作为阶段 A 的审查依据，但 **Issue #4 应继续 Open**，待两个 PARTIAL 补证以及阶段 B 在 #11 目标架构下完成。
