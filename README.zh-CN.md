<h1 align="center">
  <a href="https://krill.page/en"><img src="https://krill.page/icon-192.png" alt="Krill" width="56" valign="middle" /></a> Krill
</h1>

<p align="center">
  <a href="https://github.com/craftowen/krill/stargazers"><img src="https://img.shields.io/github/stars/craftowen/krill?style=flat&amp;label=%E2%98%85&amp;color=ff5238" alt="GitHub Star 数" /></a>
  <a href="https://github.com/craftowen/krill/releases/latest"><img src="https://img.shields.io/github/v/release/craftowen/krill?style=flat&amp;color=2a3040" alt="最新发布版本" /></a>
  <a href="LICENSE"><img src="https://img.shields.io/badge/license-Apache--2.0-3b82f6?style=flat" alt="许可证：Apache-2.0" /></a>
  <img src="https://img.shields.io/badge/macOS%20Apple%20Silicon%20%7C%20Windows%20x64%20%7C%20Mobile%20PWA-10b981?style=flat" alt="支持平台：macOS Apple Silicon、Windows x64 预览版、移动端 PWA" />
</p>

<p align="center">
  <sub><a href="README.md">English</a> · <a href="README.ko.md">한국어</a> · <a href="README.ja.md">日本語</a> · <a href="https://krill.page/en">网站</a> · <a href="https://krill.page/en/docs">CLI 参考</a> · <a href="GETTING_STARTED.md">快速开始</a> · <a href="https://github.com/craftowen/krill/releases/latest">发布版本</a></sub>
</p>

<p align="center">
  <strong>在原生应用中并行运行多个 CLI 编程 Agent，不用 Electron，也没有 Web 视图。</strong><br/>
  Claude Code、Codex、Antigravity、Cursor 和 OpenCode 各自拥有独立的 Git worktree 和终端，并提供等待你处理的 Agent 队列和 diff 审查。在我们最新的 10 终端基准测试中（2026-10-28，Krill 0.2.20 对比 Orca 1.4.242，屏幕开启），Krill 的内存占用为 <strong>153.3 MiB</strong>，而 Orca 为 <strong>1,153.6 MiB</strong>，约为 1/7。
</p>

<p align="center">
  <a href="https://krill.page/en"><img src="https://krill.page/app-screenshot-en.png" alt="Krill 原生英文界面预览，展示临时仓库中的普通终端和真实 Git 变更" width="960" /></a>
  <br/>
  <sub>原生英文界面预览，展示临时 Git 仓库中的普通终端（已安装的 macOS 0.1.37）。另请参阅<a href="https://krill.page/app-screenshot.png">韩文界面下的真实 Claude Code 会话</a>。</sub>
</p>

---

## 为什么选择 Krill，而不是 Orca（Electron）和 `tmux`？

[Orca](https://github.com/stablyai/orca) 这类 Agent 工作区已经能在应用重启后保持终端会话，也能管理 Git worktree。**应用重启后会话仍然存活只是基本要求，并不是差异所在。**

差异在于**资源效率和进程架构**。Orca 基于 Electron（Chromium + Node.js），会启动独立的主进程、GPU 进程、渲染进程和守护进程，并在渲染进程中为每个打开的标签页保留一个终端实例。Krill 以**单一 Rust（`gpui`）UI 进程，不使用 Electron 或 Web 视图**的方式提供同样的并行 Agent 工作流：UI 渲染状态只附加到可见窗格，终端状态则保存在单个轻量守护进程（`krilld`）中，其背后是不会 panic 的 PTY 父进程（`krill-holder`）。

### 重新测量：Krill 0.2.20 vs. Orca 1.4.242（2026-10-28，屏幕开启）

测试工具和机器与下文 2026-10-25 的测试相同，但显示器**开启**（窗口可见），且机器较为繁忙（运行期间的平均负载为 11–52）。Krill 在第一次运行后从 0.2.19 自动更新到 0.2.20；所有 10 终端和 20 终端的 Krill 运行都使用 0.2.20。数值为 3 次运行的中位数。

| 指标（10 个终端） | Krill 0.2.20 | Orca 1.4.242 | Krill 与 Orca 之比 |
|---|---:|---:|---|
| 内存（`phys_footprint`，所有应用和守护进程） | **153.3 MiB** | 1,153.6 MiB | 约 1/7 |
| 空闲 CPU（30 s 平均，屏幕开启） | **1.9%** | 12.9% | 约 1/7 |
| 安装大小（`.app` 包） | **95.6 MiB** | 704.7 MiB | 约 1/7 |

- 20 个终端：内存 309.4 vs 1,347.6 MiB。1 个终端：125.4 vs 853.7 MiB。
- 屏幕开启时，`krill-app` 自身占用约 111 MiB（在之前屏幕锁定的测试中为 30 MiB），因此内存差距没有达到 1/15。

### 早前测试：实测基准 Krill 0.2.2 vs. Orca 1.4.237（2026-10-25）

在同一台 **Mac mini（Apple M6，32 GB，macOS 27.0.1）** 上测量，使用未经修改的 `/Applications` 发布版本、全新的隔离配置文件和相同的 `1200×760pt` 窗口（实际为 `1176×746`），取 18/18 次有效运行中每组 3 次运行的中位数（每个终端输出 20,000 行、~2.8 MB 的韩文、ANSI 颜色和 >120 列的文本；屏幕锁定）：

| 指标（10 个终端，3 次运行中位数） | Krill 0.2.2 (Rust · GPUI) | Orca 1.4.237 (Electron) | Krill 与 Orca 之比 |
|---|---:|---:|---|
| **内存（`phys_footprint`，所有应用和守护进程）** | **85.9 MiB** | 1,274.9 MiB | **1/15**（`1 / 14.8`） |
| **空闲 CPU（输出结束后 30s 平均，屏幕锁定）** | **1.13%** | 31.0% | **1/27**（`1 / 27.4`） |
| **输出处理 CPU 时间（10 × 2.8 MB 突发）** | **1.22 s** | 6.44 s | **1/5**（`1 / 5.3`） |
| **安装大小（磁盘上的 `.app` 包）** | **92.1 MiB** | 689.1 MiB | **1/7**（`1 / 7.5`） |

#### 内存都用在了哪里（10 个终端，`phys_footprint` 中位数）

| 进程组 | Krill 0.2.2 | Orca 1.4.237 | 差异原因 |
|---|---:|---:|---|
| **应用与 UI 渲染** | **30.2 MiB** (`krill-app`) | **994.3 MiB** (`GPU 522.9` + `Main 259.3` + `Renderer 212.1`) | 单一原生 Rust GPUI 进程 vs. Chromium 主进程 + GPU 进程 + 渲染进程 |
| **终端守护进程** | **46.5 MiB** (`krilld 44.9` + `holder 1.6`) | **256.3 MiB** (`orca-terminal-host`) | 1,000 行热数据 + `zstd` 冷回滚缓冲 vs. Node/JS 终端宿主 |
| **其他辅助进程** | **9.2 MiB** (`krill-link serve`) | **24.1 MiB** (`parcel-watcher 14.0` + `Network 7.6` + `crashpad 2.5`) | 内置移动端/隧道网关 vs. Electron 工具辅助进程 |
| **合计（不含 shell）** | **85.9 MiB** | **1,274.9 MiB** | **Krill 的内存占用为 1/15（`1 / 14.8`）** |

#### 随终端数量的变化（1 · 10 · 20 个终端，3 次运行中位数）

| 指标 | 终端数 | Krill 0.2.2 | Orca 1.4.237 | Krill / Orca |
|---|---:|---:|---:|---|
| **内存（`phys_footprint`）** | 1 | **56.6 MiB** | 844.9 MiB | `1 / 14.9`（1/15） |
| | 10 | **85.9 MiB** | 1,274.9 MiB | `1 / 14.8`（**1/15**） |
| | 20 | **115.0 MiB** | 1,382.7 MiB | `1 / 12.0`（**1/12**） |
| **空闲 CPU（30s 平均，屏幕锁定）** | 1 | **0.97%** | 32.6% | `1 / 33.6`（1/34） |
| | 10 | **1.13%** | 31.0% | `1 / 27.4`（**1/27**） |
| | 20 | **1.17%** | 29.0% | `1 / 24.8`（**1/25**） |
| **输出处理 CPU 时间（2.8 MB × N）** | 1 | **0.05 s** | 1.07 s | `1 / 21.4` |
| | 10 | **1.22 s** | 6.44 s | `1 / 5.3`（**1/5**） |
| | 20 | **2.91 s** | 7.69 s | `1 / 2.6` |

<details>
<summary><strong>测量条件与已知限制（请与数字一并阅读）</strong></summary>

- **相同的环境**：未经修改的 `/Applications` 应用包（`Krill 0.2.2`、`Orca 1.4.237`），运行于 Mac mini（Apple M6，32 GB，macOS 27.0.1，2026-10-25）。每次运行使用全新的隔离配置文件、`1200×760pt` 窗口（实际为 `1176×746`），在同一个只有 1 个提交的 Git 仓库中打开 `N` 个登录 shell（`/bin/zsh -l`）标签页，其中 1 个标签页可见（全部 18 次运行的 `orca_orphaned_terminals = 0`）。
- **相同的负载与统计方式**：每个标签页输出 20,000 行（~2.8 MB 的韩文、ANSI 颜色和 >120 列的行）。静置 30s 后，以 5s 为间隔对 `phys_footprint` 采样 3 次，再将每个进程的中位数相加，范围涵盖所有应用、辅助、GPU、渲染和守护进程（包括 `krill-link serve`）。两边都排除了登录 shell 和 `perl` 生成器。
- **屏幕锁定状态**：全部 18 次运行都在显示器锁定时记录（窗口未合成到屏幕上）。即使显示器锁定，Orca 的 GPU 进程仍占用~21–24% 的 CPU，渲染进程~7.5%。解锁或窗口可见时的 CPU 属于另一项测量，这里不作任何声明。
- **共享机器负载**：测试在同时运行后台任务的共享工作站上进行（1 分钟平均负载 `15.0–184.8`，每轮重复交替应用顺序）。内存（`phys_footprint`）基本不受后台负载影响，而 CPU 和突发的实际耗时可能会波动。
- **架构差异**：Krill 只把可见窗格连接到守护进程，并把终端状态保存在一个守护进程（`krilld`）中；Orca 则在渲染进程中为每个标签页保留一个终端实例，同时在 `orca-terminal-host` 中保存状态。这里测量的是用户打开 `N` 个标签页时的实际资源占用。Agent 进程（`claude`、`codex` 等）不计入，因为无论使用哪个宿主应用，它们都是相同的。

</details>

---

## 功能

<table>
<tr>
<td width="50%" valign="top">

### 1. 比应用活得更久的会话
退出、崩溃或更新 `krill-app` 时，Agent 仍在后台继续工作。重新打开应用（或运行 `krill attach <id>`）即可回放保留的关键帧和 `zstd` 回滚缓冲片段；如果超出保留范围，会显示明确的 `Gap` 标记。

[架构 §4.1 →](docs/ARCHITECTURE.md#41-장애별-보장-범위-규범)

</td>
<td width="50%" valign="top">

### 2. 并行 Git Worktree 与 Diff 审查
从侧边栏或通过 `krill worktree new` 为每个任务创建独立的 Git worktree，运行仓库设置脚本前先审查，在 5 个审查标签页（`Changes`、`Files`、`Search`、`Checks`、`History`）中检查变更，并在终端旁直接添加行内 diff 评论。

[快速开始 →](GETTING_STARTED.md)

</td>
</tr>
<tr>
<td width="50%" valign="top">

### 3. 等待中的 Agent 优先（`Needs You`）
内置生命周期 hook（`krill hook install`）跟踪 6 种状态：`working`、`waiting`、`done`、`blocked`、`idle` 和 `exited`，并通过 **Needs You** 队列顶部、macOS 通知和 Dock 角标提示需要你输入的 Agent。

[CLI Hook 参考 →](https://krill.page/en/docs#c-agents)

</td>
<td width="50%" valign="top">

### 4. 多 Agent 任务 DAG 与决策关卡
让协调 Agent 拆解相互依赖的任务（`krill orchestration run-create`、`task-create`），在独立 worktree 中启动 worker Agent（`worker-start`），交换阻塞式 `ask`/`reply` 消息，并在人工决策关卡处暂停（`gate-create`）。

[编排文档 →](https://krill.page/en/docs#c-orch)

</td>
</tr>
<tr>
<td width="50%" valign="top">

### 5. 移动端 E2EE 伴侣（PWA）
在 **Settings → Mobile** 中扫描二维码，即可通过手机浏览器或主屏幕 Web 应用（PWA）监控 Agent 的实时状态并打开终端。通过 Relay 或 Tailscale 连接，并使用端到端 **ChaCha20-Poly1305**（`HKDF-SHA256`）加密。

选择托管 Relay 时，需要在桌面端使用 Google 登录。手机打开 PWA 时无需登录，通过二维码配对或已保存的设备密钥进行认证。

[移动端设置 →](https://krill.page/en/docs)

</td>
<td width="50%" valign="top">

### 6. 一键发布沙箱化 Artifact（`share.krill.page`）
将 Agent 生成的 HTML 报告、架构图和 Markdown 文档发布为单个沙箱化公开链接（可配置有效期，也可不设有效期；上限 10 MiB），并可随时用 `krill artifacts share` 更新或撤销同一 URL。

[Artifact 指南 →](docs/ARTIFACTS.md)

</td>
</tr>
</table>

**其他内置功能：**

- **无界面 `krill` CLI 与远程 SSH 主机**：无需 GUI 即可操作所有会话、worktree 和编排运行（`krill ls`、`new`、`send`、`read`、`wait`、`attach`、`handoff`），也可以跨受支持的远程 Mac 主机操作（`krill --host user@host`）。
- **账号切换与自动切换（`krill account`）**：切换 Agent 账号，或在达到用量速率限制时自动轮换账号。
- **定时 Agent 运行（`krill schedule`）**：按 UTC cron 计划运行周期性 Agent 任务（[定时任务文档](docs/SCHEDULING.md)）。
- **45 款终端主题与一流的韩文输入法支持**：内置 15 款 Krill 主题（包括 **Krill Deep**）和 30 款兼容 Orca 的内置主题（并支持导入 Ghostty/Warp 主题），以及专门的韩文输入法组字状态机。
- **快速打开与工作区上下文**：跨 worktree、会话、文件、检测到的本地服务器端口（`krill ports`）和已附加的参考 URL（`krill reference`）进行搜索。

---

## 支持的 Agent

适用于**任何 CLI Agent 或 TUI**：只要能在终端中运行，就能在 Krill 中获得持久的 PTY 保护。以下工具内置了配置文件和状态 hook（`krill hook install`）：

<p>
  <kbd>Claude Code</kbd> &nbsp;
  <kbd>OpenAI Codex</kbd> &nbsp;
  <kbd>Antigravity (agy)</kbd> &nbsp;
  <kbd>Cursor CLI</kbd> &nbsp;
  <kbd>OpenCode</kbd> &nbsp;
  <kbd>+ 任意 shell、开发服务器或 TUI</kbd>
</p>

---

## 安装发布版本

二进制安装**不需要** Rust 或从源码构建。你选择的 AI Agent 需要另行安装和认证；使用 Git worktree 需要 Git。Krill 不包含 Agent 订阅或凭据。

使用指标默认开启，且不会弹出同意对话框：随机安装 ID、首次启动、应用启动次数、每日交互、版本、操作系统和架构。提示词、终端输出、命令、项目路径和账号信息不在收集范围内。运行 `krill telemetry disable` 可停止收集；`DO_NOT_TRACK=1` 同样可以关闭。发送状态和运营方设置请参阅[使用指标](services/telemetry/CLIENT.md)。错误报告是单独的设置，默认关闭。

### macOS（Apple Silicon `arm64`）

安装程序需要 Bash、`curl`、Python 3 以及 macOS 系统自带的归档和签名工具。它会安装 `/Applications/Krill.app`，将 CLI 链接到 `~/.local/bin/krill`，然后打开应用：

```sh
curl -fsSL https://krill.page/install.sh -o /tmp/krill-install.sh
# Review the installer before running it.
bash /tmp/krill-install.sh
export PATH="$HOME/.local/bin:$PATH"
krill --help
```

macOS 版本**未经公证**。安装程序会验证归档校验和与应用签名，并移除所下载应用的隔离属性。有效的本地/ad-hoc 签名并不等于 Apple Developer ID 认可。详情请参阅[打包说明](packaging/README.md)（韩文）。

### Windows（`x64` 预览版）

在 PowerShell 中下载并审查安装程序，然后运行：

```powershell
Invoke-WebRequest https://krill.page/install.ps1 -OutFile "$env:TEMP\krill-install.ps1"
Get-Content "$env:TEMP\krill-install.ps1"
powershell -ExecutionPolicy Bypass -File "$env:TEMP\krill-install.ps1"
```

安装程序会验证 ZIP 的 SHA-256，安装到 `%LOCALAPPDATA%\Programs\Krill`，添加开始菜单快捷方式和用户 `PATH` 条目，然后打开应用。请打开新终端以使用更新后的 `PATH`，然后运行 `krill --help`。Windows Authenticode 签名仍未完成；校验和或更新签名验证并不代表应用拥有受信任的发布者签名。

[发布文件](https://github.com/craftowen/krill/releases/latest) · [Windows 打包](packaging/windows/README.md)（韩文）。请查看你所选版本附带的文件，不要根据源码目录推断可下载内容。Homebrew 工具目前只用于组装个人 tap，并不提供官方的 Homebrew 安装命令。

### 平台支持矩阵

| 平台 | 当前范围 | 安装方式 |
|---|---|---|
| **macOS，Apple Silicon（`arm64`）** | 主要桌面平台 | 发布版应用和 CLI，或从源码构建 |
| **Windows，`x64`** | 原生桌面预览版；除已记录的 Windows Server 虚拟机测试外，桌面 GUI、交互式登录、睡眠和重启行为仍需验证 | Windows 发布版 ZIP 和 PowerShell 安装程序 |
| **移动端（iOS / Android）** | 通过二维码配对的配套 Web 应用（PWA），经 Relay 或 Tailscale 连接（ChaCha20-Poly1305 E2EE） | 在 Mac 应用的 **Settings → Mobile** 中扫描二维码（无应用商店版本） |
| **macOS，Intel（`x86_64`）** | 本次公开预览不支持 | 不提供受支持的发布包，也不承诺源码安装 |
| **Linux** | 本次公开预览不支持 | 不承诺提供 Linux 桌面版或可下载的远程主机软件包 |
| **Windows 11，ARM64** | 通过 Windows 11 x64 仿真运行 `x64` 软件包；没有原生 ARM64 版本 | 相同的 Windows 发布版 ZIP 和 PowerShell 安装程序 |
| **Windows 10 on ARM / 32 位 Windows** | 不支持 | 没有发布包 |

---

## 首次成功运行会话

1. 打开 Krill，使用 **Open folder…** 选择一个本地 Git 仓库。如果界面是韩文，请选择 **설정 → 모양 → 언어 → English**（Settings → Appearance → Language）。
2. 打开 **New session**（`+`）菜单并选择 **Terminal**，或选择已安装并完成认证的 Agent，例如 Claude Code 或 Codex。让 Agent 检查这个仓库。
3. 在侧边栏使用 **New worktree**，输入一个唯一的名称，然后选择 **Create**。批准前先审查设置脚本，或选择 **Skip setup**。在那里启动第二个 Agent，让它独立工作。
4. 用 `krill ls` 记下会话 ID。保留会话标签页，退出并重新打开应用；确认运行中的会话以相同的 ID 重新连接。显式关闭会话标签页或窗格会终止该会话。

可复制的 GUI、CLI 和源码构建步骤请参阅[快速开始](GETTING_STARTED.md)。

---

## 发生故障时什么能保留下来

| 事件 | 运行中的 Agent 进程 | 输出与恢复 |
|---|---|---|
| **应用退出、崩溃或重启** | 只要会话服务仍然存活就会继续运行 | 重新连接到同一会话；保留的历史记录有上限，溢出时可能产生 `Gap` |
| **`krilld` 崩溃** | 可以继续运行，因为 holder 保留着 PTY 和句柄 | 已读取但尚未记录的字节可能丢失；恢复时会标记 `Gap` |
| **两个会话服务都停止，或机器重启** | 无法存活 | 已保存的历史记录并不是运行中的进程；恢复 Agent 对话是单独的操作 |

这是跨应用重启的连续性，并不能防止所有形式的数据丢失。各平台的恢复方式和验证限制定义于[架构 §4.1](docs/ARCHITECTURE.md#41-장애별-보장-범위-규범)。

---

## 构建与贡献

从源码构建需要 Rust **1.98 或更高版本**（MSRV 见 [Cargo.toml](Cargo.toml)，推荐 Homebrew `rustc`，不需要 `rustup`）、Git，以及 Apple Silicon 上的 macOS Command Line Tools。发布包组装脚本需要 Python 3.9+；本地从源码运行不需要完整的 Xcode 和 Apple Developer 会员资格。请按照[源码构建步骤](GETTING_STARTED.md#build-from-source-on-macos)操作。

[贡献指南](CONTRIBUTING.md)按变更范围说明了检查要求：只改 Markdown 时使用 `git diff --check`；完整的 `scripts/check.sh` 是发布候选版本的关卡。[安全策略与私密报告](SECURITY.md) · [行为准则](CODE_OF_CONDUCT.md)。

设计文档目前仍为韩文：[架构](docs/ARCHITECTURE.md)、[路线图](docs/ROADMAP.md)、[代码规范](docs/CONVENTIONS.md)、[UI 风格指南](docs/STYLEGUIDE.md)。

---

## 许可证

Apache-2.0：[LICENSE](LICENSE)，Copyright 2026 Krill contributors。请参阅[第三方声明](THIRD_PARTY_NOTICES.md)和[许可证记录](docs/licensing/README.md)。贡献时请勿阅读或复制 Zed 的 GPL-3.0 `terminal` 或 `terminal_view` 代码；如有获准复用 Orca 源码的情况，请按[贡献指南](CONTRIBUTING.md)中的说明注明出处。

使用指标默认开启，且不会弹出同意对话框：随机安装 ID、首次启动、应用启动次数、每日交互、版本、操作系统和架构。提示词、终端输出、命令、项目路径和账号信息不在收集范围内。运行 `krill telemetry disable` 可停止收集；`DO_NOT_TRACK=1` 同样可以关闭。发送状态和运营方设置请参阅[使用指标](services/telemetry/CLIENT.md)。错误报告是单独的设置，默认关闭。
