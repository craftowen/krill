# Krill — AI coding workspace

[한국어](README.ko.md) · [Website](https://krill.page/en) · [CLI reference](https://krill.page/en/docs)

**A native Rust workspace for parallel AI coding agents, with persistent terminal sessions and Git worktrees.**

Run agents in separate worktrees, inspect their output, and reconnect to running sessions after restarting the app. Krill separates the GUI (`krill-app`) from the session services (`krill-holder` and `krilld`). The `krill` CLI can manage sessions without the GUI.

## Platform support

| Platform | Current scope | Installation |
|---|---|---|
| macOS, Apple Silicon (arm64) | Primary desktop platform | Release app and CLI, or source build |
| Windows, x64 | Native desktop preview; desktop GUI, interactive logon, sleep, and reboot behavior still need validation beyond the recorded Windows Server VM tests | Windows release ZIP and PowerShell installer |
| macOS, Intel | Not supported for this public preview | No supported release bundle or source-install promise |
| Linux | Not supported for this public preview | No Linux desktop or downloadable remote-host package promised |
| Windows, ARM64 / 32-bit | Not supported | No release package |

- 지원 플랫폼: macOS Apple Silicon(M 시리즈)·Windows x64. Intel Mac·Linux는 지원하지 않습니다.
- 아래 macOS 소스 빌드에는 Command Line Tools가 필요합니다.
- Rust 1.98 이상 (MSRV는 루트 `Cargo.toml`, Homebrew rustc 권장, rustup 불필요)
- Python 3.9 이상 (릴리스 조립 스크립트용)

## Install a release

Binary installation does **not** require Rust or a source build. Install and authenticate your chosen AI agent separately; Git is needed for Git worktrees. Krill does not include agent subscriptions or credentials.

**macOS Apple Silicon:** the installer needs Bash, curl, Python 3, and the macOS system archive/signature tools. It installs `/Applications/Krill.app`, links the CLI at `~/.local/bin/krill`, and opens the app.

```sh
curl -fsSL https://krill.page/install.sh -o /tmp/krill-install.sh
# Review the installer before running it.
bash /tmp/krill-install.sh
export PATH="$HOME/.local/bin:$PATH"
krill --help
```

The macOS release is **not notarized**. The installer verifies the archive checksum and app signature and removes the downloaded app's quarantine attribute. A valid local/ad-hoc signature is not Apple Developer ID approval. See [packaging details](packaging/README.md) (Korean).

**Windows x64:** in PowerShell, download and review the installer, then run it:

```powershell
Invoke-WebRequest https://krill.page/install.ps1 -OutFile "$env:TEMP\krill-install.ps1"
Get-Content "$env:TEMP\krill-install.ps1"
powershell -ExecutionPolicy Bypass -File "$env:TEMP\krill-install.ps1"
```

The installer verifies the ZIP's SHA-256, installs under `%LOCALAPPDATA%\Programs\Krill`, adds a Start menu shortcut and user PATH entry, and opens the app. Open a new terminal for the updated PATH, then run `krill --help`. Windows Authenticode signing remains pending; checksum or update-signature verification does not mean the app has a trusted publisher signature.

[Release assets](https://github.com/craftowen/krill/releases/latest) · [Windows packaging](packaging/windows/README.md) (Korean). Check the assets attached to the release you choose; do not infer downloads from source directories. The Homebrew tooling currently assembles a personal tap, rather than providing an official Homebrew installation command.

## First successful session

1. Open Krill and use **Open folder…** to select a local Git repository. If the interface is Korean, select **설정 → 일반 · 외관 → 언어 → English** (Settings → General & Appearance → Language).
2. Open the **New session** (`+`) menu and choose **Terminal**, or choose an installed, authenticated agent such as Claude Code or Codex. Ask the agent to inspect the repository.
3. Use **New worktree** in the sidebar, enter a unique name, and choose **Create**. Review any setup script before approving it, or choose **Skip setup**. Start a second agent there to work independently.
4. Record the session IDs with `krill ls`. Quit and reopen the app while leaving the session tabs intact; check that the running sessions reconnect with the same IDs. Closing a session tab/pane explicitly terminates that session.

For copyable GUI, CLI, and source-build walkthroughs, see [Getting started](GETTING_STARTED.md).

## What survives a failure

| Event | Running agent process | Output and recovery |
|---|---|---|
| App quit, crash, or restart | Continues while the session services remain alive | Reconnect to the same session; retained history is bounded and overflow can produce a Gap |
| `krilld` crash | Can continue because the holder retains the PTY/handles | Bytes read but not yet recorded may be lost; recovery marks a Gap |
| Both session services stop, or the machine reboots | Does not survive | Saved history is not a live process; agent conversation resume is a separate action |

This is continuity across app restarts, not a guarantee against every form of data loss. Platform-specific recovery and validation limits are defined in [Architecture §4.1](docs/ARCHITECTURE.md#41-장애별-보장-범위-규범). Existing measurements and their workload limits are in [the benchmark record](docs/benchmarks/MK-074.md); they do not establish a general performance comparison or an app-plus-daemon memory result.

## Build and contribute

Source builds need Rust **1.98 or newer** (MSRV in [Cargo.toml](Cargo.toml)), Git, and macOS Command Line Tools on Apple Silicon. Python 3 is needed for packaging; a full Xcode installation and Apple Developer membership are not required for local source execution. Follow the [source walkthrough](GETTING_STARTED.md#build-from-source-on-macos).

[Contributing](CONTRIBUTING.md) explains checks by change scope: Markdown-only changes use `git diff --check`; the full `scripts/check.sh` is a release-candidate gate. [Security policy and private reporting](SECURITY.md) · [Code of conduct](CODE_OF_CONDUCT.md).

Design documents currently remain in Korean: [Architecture](docs/ARCHITECTURE.md), [Roadmap](docs/ROADMAP.md), [Code conventions](docs/CONVENTIONS.md), [UI style guide](docs/STYLEGUIDE.md).

## License

Apache-2.0 — [LICENSE](LICENSE), Copyright 2026 Krill contributors. See [third-party notices](THIRD_PARTY_NOTICES.md) and [license records](docs/licensing/README.md). Do not read or copy Zed's GPL-3.0 `terminal` or `terminal_view` code when contributing; credit any permitted Orca source reuse as described in [Contributing](CONTRIBUTING.md).
