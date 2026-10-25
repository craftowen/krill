<h1 align="center">
  <a href="https://krill.page/en"><img src="https://krill.page/icon-192.png" alt="Krill" width="56" valign="middle" /></a> Krill
</h1>

<p align="center">
  <a href="https://github.com/craftowen/krill/stargazers"><img src="https://img.shields.io/github/stars/craftowen/krill?style=flat&amp;label=%E2%98%85&amp;color=ff5238" alt="GitHub stars" /></a>
  <a href="https://github.com/craftowen/krill/releases/latest"><img src="https://img.shields.io/github/v/release/craftowen/krill?style=flat&amp;color=2a3040" alt="Latest release" /></a>
  <a href="LICENSE"><img src="https://img.shields.io/badge/license-Apache--2.0-3b82f6?style=flat" alt="License: Apache-2.0" /></a>
  <img src="https://img.shields.io/badge/macOS%20Apple%20Silicon%20%7C%20Windows%20x64%20%7C%20Mobile%20PWA-10b981?style=flat" alt="Platforms: macOS Apple Silicon, Windows x64 preview, Mobile PWA" />
</p>

<p align="center">
  <sub><a href="README.ko.md">한국어</a> · <a href="https://krill.page/en">Website</a> · <a href="https://krill.page/en/docs">CLI Reference</a> · <a href="GETTING_STARTED.md">Getting Started</a> · <a href="https://github.com/craftowen/krill/releases/latest">Releases</a></sub>
</p>

<p align="center">
  <strong>Parallel AI coding agents. Terminal sessions that outlive the app.</strong><br/>
  Run Claude Code, Codex, Antigravity, Cursor, and OpenCode side-by-side in isolated Git worktrees — powered by a 3-tier native Rust architecture (<code>krill-app</code> + <code>krilld</code> + <code>krill-holder</code>).
</p>

<p align="center">
  <a href="https://krill.page/en"><img src="https://krill.page/app-screenshot-en.png" alt="Krill native English UI preview with an ordinary terminal and real Git changes in a disposable repository" width="960" /></a>
  <br/>
  <sub>Native English UI preview with an ordinary terminal in a disposable Git repository (installed macOS 0.1.37). See also an <a href="https://krill.page/app-screenshot.png">actual Claude Code session in Korean</a>.</sub>
</p>

---

## Why Krill?

Standard terminals lose context when you juggle multiple coding agents; `tmux` lacks Git worktree isolation, diff review, and native agent notifications; Electron wrappers tie your running agent processes to a heavy GUI renderer.

Krill is built in **pure Rust (GPUI)** around a **3-tier process boundary**:
- **`krill-app` (Native UI)** — Renders only visible rows from snapshots and deltas (target budget ≤ 120 MB). Close, crash, or update the app without killing your agents.
- **`krilld` (Session Daemon)** — The single terminal query responder. Maintains a 1,000-row hot grid plus `zstd` cold scrollback segments (target budget ≤ 60 MB at 10 terminals; design targets, not a universal release benchmark).
- **`krill-holder` (PTY Parent)** — A tiny, panic-free supervisor (≤ 1,300 lines on macOS) that owns the PTY file descriptors and hands them to `krilld` over `SCM_RIGHTS`.

---

## Features

<table>
<tr>
<td width="50%" valign="top">

### 1. Sessions That Outlive the App
Quit, crash, or update `krill-app` while your agents keep working in the background. Reopen the app (or run `krill attach <id>`) to replay retained keyframes and `zstd` scrollback segments, with explicit `Gap` markers if retention overflows.

[Architecture §4.1 →](docs/ARCHITECTURE.md#41-장애별-보장-범위-규범)

</td>
<td width="50%" valign="top">

### 2. Parallel Git Worktrees &amp; Diff Review
Create an isolated Git worktree per task from the sidebar or via `krill worktree new`, review repository setup scripts before running them, inspect changes across 5 review tabs (`Changes`, `Files`, `Search`, `Checks`, `History`), and drop inline diff comments right beside the terminal.

[Getting Started →](GETTING_STARTED.md)

</td>
</tr>
<tr>
<td width="50%" valign="top">

### 3. Waiting Agents First (`Needs You`)
Built-in lifecycle hooks (`krill hook install`) track 6 states — `working`, `waiting`, `done`, `blocked`, `idle`, and `exited` — surfacing agents that need your input at the top of the **Needs You** queue, macOS notifications, and Dock badges.

[CLI Hooks Reference →](https://krill.page/en/docs#c-agents)

</td>
<td width="50%" valign="top">

### 4. Multi-Agent Task DAGs &amp; Decision Gates
Let a coordinator agent break down dependent tasks (`krill orchestration run-create`, `task-create`), spawn worker agents in isolated worktrees (`worker-start`), exchange blocking `ask`/`reply` messages, and pause at human decision gates (`gate-create`).

[Orchestration Docs →](https://krill.page/en/docs#c-orch)

</td>
</tr>
<tr>
<td width="50%" valign="top">

### 5. Mobile E2EE Companion (PWA)
Scan a QR code in **Settings → Mobile** to monitor live agent states and open terminals from your phone browser or Home Screen web app (PWA). Connects over Relay or Tailscale with end-to-end **ChaCha20-Poly1305** (`HKDF-SHA256`) encryption.

[Mobile Setup →](https://krill.page/en/docs)

</td>
<td width="50%" valign="top">

### 6. 1-Click Sandboxed Artifacts (`share.krill.page`)
Publish agent-generated HTML reports, architecture diagrams, and Markdown documents as a single sandboxed public link (90-day expiry, 2 MB limit) and update or revoke the same URL anytime with `krill artifacts share`.

[Artifacts Guide →](docs/ARTIFACTS.md)

</td>
</tr>
</table>

**Also in the box:**

- **Headless `krill` CLI &amp; Remote SSH Hosts** — Drive every session, worktree, and orchestration run without the GUI (`krill ls`, `new`, `send`, `read`, `wait`, `attach`, `handoff`) or across supported remote Mac hosts (`krill --host user@host`).
- **Account Switcher &amp; Auto-Switch (`krill account`)** — Switch agent accounts or automatically rotate accounts when usage rate limits are reached.
- **Scheduled Agent Runs (`krill schedule`)** — Run recurring agent tasks on a UTC cron schedule ([Scheduling docs](docs/SCHEDULING.md)).
- **31 Terminal Themes &amp; First-Class Korean IME** — Ships with **Krill Deep** plus 30 built-in themes (and Ghostty/Warp theme import) and a dedicated Hangul IME composition state machine.
- **Quick Open &amp; Workspace Context** — Search across worktrees, sessions, files, detected local server ports (`krill ports`), and attached reference URLs (`krill reference`).

---

## Supported Agents

Works with **any CLI agent or TUI** — if it runs in a terminal, it gets persistent PTY protection in Krill. Built-in profiles and status hooks (`krill hook install`) are included for:

<p>
  <kbd>Claude Code</kbd> &nbsp;
  <kbd>OpenAI Codex</kbd> &nbsp;
  <kbd>Antigravity (agy)</kbd> &nbsp;
  <kbd>Cursor CLI</kbd> &nbsp;
  <kbd>OpenCode</kbd> &nbsp;
  <kbd>+ any shell, dev server, or TUI</kbd>
</p>

---

## Install a Release

Binary installation does **not** require Rust or a source build. Install and authenticate your chosen AI agent separately; Git is needed for Git worktrees. Krill does not include agent subscriptions or credentials.

### macOS (Apple Silicon `arm64`)

The installer requires Bash, `curl`, Python 3, and the macOS system archive/signature tools. It installs `/Applications/Krill.app`, links the CLI at `~/.local/bin/krill`, and opens the app:

```sh
curl -fsSL https://krill.page/install.sh -o /tmp/krill-install.sh
# Review the installer before running it.
bash /tmp/krill-install.sh
export PATH="$HOME/.local/bin:$PATH"
krill --help
```

The macOS release is **not notarized**. The installer verifies the archive checksum and app signature and removes the downloaded app's quarantine attribute. A valid local/ad-hoc signature is not Apple Developer ID approval. See [packaging details](packaging/README.md) (Korean).

### Windows (`x64` Preview)

In PowerShell, download and review the installer, then run it:

```powershell
Invoke-WebRequest https://krill.page/install.ps1 -OutFile "$env:TEMP\krill-install.ps1"
Get-Content "$env:TEMP\krill-install.ps1"
powershell -ExecutionPolicy Bypass -File "$env:TEMP\krill-install.ps1"
```

The installer verifies the ZIP's SHA-256, installs under `%LOCALAPPDATA%\Programs\Krill`, adds a Start menu shortcut and user `PATH` entry, and opens the app. Open a new terminal for the updated `PATH`, then run `krill --help`. Windows Authenticode signing remains pending; checksum or update-signature verification does not mean the app has a trusted publisher signature.

[Release assets](https://github.com/craftowen/krill/releases/latest) · [Windows packaging](packaging/windows/README.md) (Korean). Check the assets attached to the release you choose; do not infer downloads from source directories. The Homebrew tooling currently assembles a personal tap, rather than providing an official Homebrew installation command.

### Platform Support Matrix

| Platform | Current scope | Installation |
|---|---|---|
| **macOS, Apple Silicon (`arm64`)** | Primary desktop platform | Release app and CLI, or source build |
| **Windows, `x64`** | Native desktop preview; desktop GUI, interactive logon, sleep, and reboot behavior still need validation beyond the recorded Windows Server VM tests | Windows release ZIP and PowerShell installer |
| **Mobile (iOS / Android)** | Companion web app (PWA) paired via QR code over Relay or Tailscale (ChaCha20-Poly1305 E2EE) | Scan QR from **Settings → Mobile** in the Mac app (no app-store binary) |
| **macOS, Intel (`x86_64`)** | Not supported for this public preview | No supported release bundle or source-install promise |
| **Linux** | Not supported for this public preview | No Linux desktop or downloadable remote-host package promised |
| **Windows, ARM64 / 32-bit** | Not supported | No release package |

---

## First Successful Session

1. Open Krill and use **Open folder…** to select a local Git repository. If the interface is Korean, select **설정 → 일반 · 외관 → 언어 → English** (Settings → General & Appearance → Language).
2. Open the **New session** (`+`) menu and choose **Terminal**, or choose an installed, authenticated agent such as Claude Code or Codex. Ask the agent to inspect the repository.
3. Use **New worktree** in the sidebar, enter a unique name, and choose **Create**. Review any setup script before approving it, or choose **Skip setup**. Start a second agent there to work independently.
4. Record the session IDs with `krill ls`. Quit and reopen the app while leaving the session tabs intact; check that the running sessions reconnect with the same IDs. Closing a session tab/pane explicitly terminates that session.

For copyable GUI, CLI, and source-build walkthroughs, see [Getting started](GETTING_STARTED.md).

---

## What Survives a Failure

| Event | Running agent process | Output and recovery |
|---|---|---|
| **App quit, crash, or restart** | Continues while the session services remain alive | Reconnect to the same session; retained history is bounded and overflow can produce a `Gap` |
| **`krilld` crash** | Can continue because the holder retains the PTY/handles | Bytes read but not yet recorded may be lost; recovery marks a `Gap` |
| **Both session services stop, or the machine reboots** | Does not survive | Saved history is not a live process; agent conversation resume is a separate action |

This is continuity across app restarts, not a guarantee against every form of data loss. Platform-specific recovery and validation limits are defined in [Architecture §4.1](docs/ARCHITECTURE.md#41-장애별-보장-범위-규범).

---

## Build and Contribute

Source builds need Rust **1.98 or newer** (MSRV in [Cargo.toml](Cargo.toml), Homebrew `rustc` recommended, `rustup` not required), Git, and macOS Command Line Tools on Apple Silicon. Python 3.9+ is needed for release assembly scripts; a full Xcode installation and Apple Developer membership are not required for local source execution. Follow the [source walkthrough](GETTING_STARTED.md#build-from-source-on-macos).

[Contributing](CONTRIBUTING.md) explains checks by change scope: Markdown-only changes use `git diff --check`; the full `scripts/check.sh` is a release-candidate gate. [Security policy and private reporting](SECURITY.md) · [Code of conduct](CODE_OF_CONDUCT.md).

Design documents currently remain in Korean: [Architecture](docs/ARCHITECTURE.md), [Roadmap](docs/ROADMAP.md), [Code conventions](docs/CONVENTIONS.md), [UI style guide](docs/STYLEGUIDE.md).

---

## License

Apache-2.0 — [LICENSE](LICENSE), Copyright 2026 Krill contributors. See [third-party notices](THIRD_PARTY_NOTICES.md) and [license records](docs/licensing/README.md). Do not read or copy Zed's GPL-3.0 `terminal` or `terminal_view` code when contributing; credit any permitted Orca source reuse as described in [Contributing](CONTRIBUTING.md).
