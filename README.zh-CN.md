<p align="center">
  <img src="assets/icon.png" width="80" alt="Agent Toolchain Manager" />
</p>

<h1 align="center">Agent Toolchain Manager</h1>

<p align="center">
  以项目为中心，统一管理可复用的 AI Agent 能力。
</p>

<p align="center">
  <a href="./README.md">English</a>
  &nbsp;·&nbsp;
  <a href="https://github.com/wzwu0309/agent-toolchain-manager/issues">问题与开发进展</a>
</p>

## 项目愿景

我们希望不同 AI 编码 Agent 能够在不同项目中获得**恰好需要的能力**，而不必为每个工具、每个仓库反复维护互不关联的配置。

**Agent Toolchain Manager** 的长期定位是一个跨 Agent 的能力资产管理与配置入口：统一整理可复用能力，按项目和 Agent 选择需要的内容，并以可检查、可追踪、可恢复的方式部署与维护。

理想中的流程是：

**发现能力 → 统一整理 → 选择项目与 Agent → 检查变更 → 部署 → 追踪与更新。**

目标不是让所有 Agent 拥有相同的配置，而是让每个项目的 Agent 工具链**明确、可复现、可审查且可安全调整**。

## 当前已具备的能力

现阶段产品主要围绕 **Skills** 展开：

- **中央 Skill 库**：从 Git 仓库、本地目录、压缩包及 [skills.sh](https://skills.sh) 市场导入技能。默认存储目录为 `~/.agent-toolchain-manager`，也可自行调整。
- **Agent 与项目工作区**：检查全局和项目级 Skills，面向所选 Agent 以复制或软链接方式部署。
- **Preset 与组织能力**：用预设组合、标签和过滤器管理多个 Skill 及其部署。
- **GUI 与 CLI**：人可以通过桌面应用操作，Codex 等 Agent 可以通过 `agent-toolchain-manager-cli` 和内置 [`manage-skills`](skills/manage-skills/SKILL.md) Skill 调用。
- **历史与备份**：提供 Git 备份、快照、多设备同步及冲突处理功能。
- **状态检查**：预览 Skill 文档、来源与部署状态，并导出应用日志。

这些是**现有的 Skill 管理能力**；不代表通用 MCP、Plugin 或命令资产管理已经实现。

## 后续希望实现的方向

以下均为**规划方向，不代表当前已交付或承诺具体完成时间**：

1. **项目优先的能力配置**：为指定仓库选择 Agent 及所需能力，不影响无关项目或未选择的全局 Agent 目录。
2. **可信版本追踪与安全更新**：明确部署内容、来源版本、项目本地修改及外部能力接管授权，避免无提示覆盖。
3. **扩展能力资产类型**：在定义清楚权限、归属和生命周期后，探索将 MCP Server、命令、Prompt 等纳入统一管理。
4. **GUI 与 Agent 自动化一致**：桌面端和 CLI 复用相同的验证、冲突判断与部署规则。
5. **可复现的 Agent 工具链**：降低跨项目、跨设备复现 Agent 能力配置的成本。

具体优先级、设计取舍及开发进展请参见 [GitHub Issues](https://github.com/wzwu0309/agent-toolchain-manager/issues)。

## 从源码构建本 Fork

本 Fork 尚无经过独立签名的正式安装包。上游 Homebrew/Scoop/Release
下载命令安装的是 Skills Manager 原版，而不是 Agent Toolchain Manager。

在配置 Rust、Cargo、Node 及 Tauri 构建依赖后：

    npm ci
    npm run tauri:build

安装前参见 [Fork 发布隔离说明](FORK_RELEASE_ISOLATION.md)。

## 快速上手

1. 从本地目录、Git 仓库、压缩包或市场安装 Skills。
2. 从侧边栏进入 **全局工作区**，选择一个 Agent（如 Claude Code）。
3. 点击 **Preset** 标签为该 Agent 一键激活对应 Skills，或点 **+ 添加 Skills** 从技能库挑选并即时切换目标 Agent。激活的 Preset 显示 ✓，部分安装显示计数角标。
4. 如需管理项目本地 Skills，打开 **项目工作区**，同样使用 Preset 标签，或通过 **+ 添加 Skills** 弹层用多 Agent 目标选择器挑选。
5. 在 **设置** 中配置 Agent 路径、自定义工具、主题、语言、代理和 Git 偏好。
6. 如果需要历史版本或多机同步，从侧边栏打开 **备份** 页，点击 **使用 GitHub 登录**——之后备份和跨设备同步都会自动进行。

## 让你的 Agent 来管理 Skills

Claude Code、Codex、Cursor 等可以替你装一个 Skill、把它部署到另一个 Agent、或者报告哪个 Agent 有什么——做法是驱动 Agent Toolchain Manager，而不是绕过它直接往 Agent 目录里写。这正是来源元数据、Preset 归属、更新追踪和跨 Agent 部署状态得以保全的原因。

Dashboard 上有一个一次性入口：勾选哪些 Agent 可以这么做，应用就把 [`manage-skills`](skills/manage-skills/SKILL.md) 装进技能库并只部署给这些 Agent。之后它就是一个普通的库内 Skill——增减 Agent 走它自己卡片上的 Agent 图标行。全程不需要配置 PATH：应用会把自己那份 CLI 发布到 Agent 会去找的位置。

它同时也是一个正常发布的 Skill，不装应用也能直接装：

```bash
npx skills add wzwu0309/agent-toolchain-manager
```

## 备份与多设备同步

侧边栏的 **备份** 页把技能库托管在一个 Git 仓库里：单台设备是带版本历史、可恢复快照的备份；多台设备连接同一仓库时会自动保持一致。远端始终是纯 Git 仓库——随时可以 `git clone` 走，没有锁定。

### 连接

- **使用 GitHub 登录**（推荐）：输入 8 位码完成授权，应用会自动创建私有仓库 `skills-manager-backup`。令牌只存在系统钥匙串里，绝不落入文件或仓库配置。
- **高级方式**：在 **设置 → Git 同步配置** 粘贴任意 Git 地址（HTTPS + PAT、SSH、自建服务均可）。
- 新机器上技能库为空时，首次启动会询问：**全新开始，还是从备份恢复？**

### 同步如何工作

- **全自动**：本地改动停止编辑约两分钟后自动提交并上传；其他设备推送的更新会自动合并进来并推送回去。随时可点 **立即备份**，备份历史会显示每一条来自哪台设备。
- **按技能合并**：同步以技能为单位而非文本行——一台设备改名、另一台改内容，会自动正确组合。
- **冲突不阻塞、不覆盖**：同一技能在两台设备被同时修改时，其余技能照常同步，该技能保留本机版本并进入 **需要处理** 列表（技能卡上也有徽章）。三选一：**保留本机 / 使用远端 / 两个都保留**——应用任一选择前都会先建安全快照，每个决定都可撤销。
- **快照与恢复**：手动备份会创建快照版本，在备份页历史中可恢复任意一个；恢复前会先把当前状态存为新快照。

### 备份包含什么

技能文件、标签、Preset 及每个 Agent 的技能开关会被备份。机密信息（API Key、令牌、代理配置）和本机接线永不上传。超过 100 MB 的技能自动留在本机、不进备份（备份页会标注）。SQLite 数据库不进 Git——其中的元数据可从技能文件重建。

### 断开连接

备份页提供三档：**断开本机**（其他设备与远端数据不受影响）、**撤销 GitHub 授权**、以及 **删除远端备份**（经 GitHub 原生的输入仓库名确认流程）。

## 支持的工具

开箱支持 56 个 Agent，包括：

Claude Code · Codex · Cursor · GitHub Copilot · Gemini CLI · GitLab Duo · OpenCode · OpenClaw · Hermes Agent · OpenHands · Cline · Goose · Windsurf · Continue · Grok · Antigravity · Qwen Code · ZCode · Crush · Kilo Code · Roo Code · Amp · Kiro CLI · Droid · TRAE IDE · Warp · Qoder · CodeBuddy · MiniMax Code

**设置**页会列出全部，并优先展示在你机器上检测到的那些。你也可以在那里添加自定义工具，以相同方式管理其 Skills。

## 技术栈

| 层 | 技术 |
|----|------|
| 前端 | React 19、TypeScript、Vite、Tailwind CSS |
| 桌面 | Tauri 2 |
| 后端 | Rust |
| 存储 | SQLite（`rusqlite`） |
| 国际化 | react-i18next |

## 快速开始

### 前置依赖

- Node.js 20.19+ 或 22.12+（Vite 7 的要求）
- Rust 1.77.2 或更高
- 当前系统的 [Tauri 依赖](https://v2.tauri.app/start/prerequisites/)

### 开发

```bash
npm install
npm run tauri:dev
```

### CLI

仓库包含一个面向 agent 的 CLI，和桌面应用共用同一套 Rust core——同一个 SQLite 数据库、同一个中央技能库、同一套同步引擎。

```bash
# 看一眼现状
npm run cli -- skills list
npm run cli -- skills show db

# 装进技能库（默认不会自动部署到任何 Agent）
npm run cli -- skills install ./my-skill
npm run cli -- skills install https://github.com/foo/bar/tree/main/skills/baz
npm run cli -- skills install vercel-labs/agent-skills@react-best-practices

# 部署给该有它的 Agent，然后核对
npm run cli -- skills deploy react-best-practices --agent claude_code --agent codex
npm run cli -- skills status react-best-practices

# 拉上游更新；把 Agent 里已有的技能纳管进来
npm run cli -- skills check --all
npm run cli -- skills update --all
npm run cli -- skills adopt ~/.claude/skills --dry-run
```

任何命令组或子命令加 `--help` 都会打印完整用法——下面几个组能做的远不止上面这些例子。部分命令支持 `--dry-run`，包括 `skills deploy/undeploy/sync/remove/adopt` 和 `presets deploy/undeploy/delete`；使用前请查看对应子命令的帮助。实际执行 `skills remove` 还需要 `--yes`。

可用命令分组：
- `repo`：查看或修改当前 base directory
- `agents`（兼容别名 `tools`）：列出 Agent、全局启用或禁用 Agent，以及添加自定义 Agent（`add-custom`）
- `skills`：管理中央库、标签，以及 skill 在各 Agent 中的真实部署
- `presets`：创建、修改、删除、整理、部署或撤下 Preset
- `git`：操作 git 管理的 `skills/` 仓库（`clone`、`pull`、`push`、`commit`、`versions`、`restore`）

额外参数：
- `--skills-root <path>`：直接针对某个已 clone / 已导出的 skills repo 操作，而不是本机 app 默认目录。manager 的状态（DB、scenarios、cache、logs）会落在 `~/.agent-toolchain-manager/external/<name>-<hash>/`，按 skills root 的规范化路径分目录隔离，外部仓库本身保持干净。
- `--json`：给脚本 / agent 使用的机器可读输出。失败时在 stderr 打印 `{"ok": false, "code": …, "message": …}` 并以非零码退出。因目标不属于我们而被拒绝的部署会把路径作为数据带出来（`code: "TARGET_CONFLICT"`、`details.conflicts[].path`），调用方可以直接指出是哪个目录挡路，而不是转述一句话。

```bash
npm run -s cli -- --skills-root /path/to/my-skills --json skills list
```

#### Agent 在 WSL 里

Windows 版应用无法在 WSL 里建链接，只能复制。如果你的 agent 都装在 WSL 里，请在 WSL 内直接使用 Linux 版 CLI：它在 WSL 里维护自己的技能库，并用真正的软链接部署：

```bash
# Build the CLI from this fork on Linux, with Rust/Cargo and Node:
npm run cli:build
# Place src-tauri/target/debug/agent-toolchain-manager-cli on PATH.
agent-toolchain-manager-cli agents add-custom hermes-work --path ~/.hermes/profiles/work/skills
agent-toolchain-manager-cli skills install ./my-skill
agent-toolchain-manager-cli skills deploy my-skill --agent hermes-work
```

#### 二进制放在哪

应用启动时会把自己那份 CLI 复制到 `~/.agent-toolchain-manager/bin/agent-toolchain-manager-cli`，版本永远与正在运行的应用一致，Agent 不需要你改 PATH 就能找到它。旁边的 `.version` 标记只在副本校验通过后才写、每次重新发布前先删——所以一次失败的复制（比如 Windows 上二进制正被占用）绝不会被当成可用的。

把 CLI 放到**你自己**的 PATH 上手敲命令，是另一件事：

```bash
npm run cli:install
# 等价于：
# cargo install --path src-tauri --bin agent-toolchain-manager-cli --locked --force
```

二进制会装到 `~/.cargo/bin/agent-toolchain-manager-cli`。代码更新后再跑一次即可刷新。

此 Fork 暂不发布独立签名 CLI，请勿从上游获取同名二进制。未来只应下载本仓库发布的 `agent-toolchain-manager-cli-*`，在 macOS/Linux 添加可执行权限后放入 PATH 即可。

#### 与桌面应用并发使用

CLI 和桌面应用共享同一个 SQLite 数据库及仓库锁。CLI 修改 metadata 或 Agent 部署后，桌面应用通常会通过文件监听自动刷新；如果应用当时处于休眠状态，手动刷新一次即可。

### 构建

```bash
npm run tauri:build
npm run cli:build
```

## 常见问题

**macOS 无法打开自行构建的 App。** 本 Fork 目前尚无独立签名、公证的正式安装包。请先确认应用来自可信的源码和构建过程，再根据 macOS **系统设置 → 隐私与安全性** 中的提示处理。上游应用历史版本的签名与公证信息不适用于本 Fork。

遇到其他问题，请在 [GitHub Issues](https://github.com/wzwu0309/agent-toolchain-manager/issues) 反馈，并附上 **设置 → 导出日志** 生成的文件。

## 项目来源与许可

Agent Toolchain Manager 基于开源项目 [Skills Manager](https://github.com/xingkongliang/skills-manager) Fork 开发。感谢原项目在 MIT 许可证下提供的基础实现；上游项目的宣传视频、热度统计与历史发布成就不应视为本项目的成果。

本仓库使用 **MIT License**，详见 [LICENSE](LICENSE)。
