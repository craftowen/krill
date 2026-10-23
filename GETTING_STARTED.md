# Getting started with Krill

[한국어](GETTING_STARTED.ko.md) · [README](README.md)

## Choose a path

For an installed release, start with the GUI or CLI steps below. Rust is unnecessary for binary users. Use [the README's installation instructions](README.md#install-a-release) for Apple Silicon macOS or the Windows x64 preview. An AI agent must be installed and authenticated separately; you can try a shell session without one. Use a local Git repository with at least one commit for the worktree walkthrough.

## First GUI workflow

1. Launch **Krill** from Applications on macOS or the Start menu on Windows.
2. If necessary, switch to English in **설정 → 일반 · 외관 → 언어 → English** (Settings → General & Appearance → Language). Saved language choices are retained.
3. Choose **Open folder…** in the sidebar and open your local Git repository. Select that workspace.
4. Open **New session** (`+`) and choose **Terminal**. Run `git status` to confirm the directory. Choose an installed agent from the same menu to create its session, then ask it to describe the repository.
5. In the sidebar, choose **New worktree**, enter a unique name such as `first-review`, and choose **Create**. Review any setup hook before approval or choose **Skip setup**. Select the new worktree and start another terminal or agent; it has a separate working directory and branch.
6. Run `krill ls` in an external terminal and note the session IDs. On macOS, quit the app with Command-Q; on Windows use **Quit Krill**. Reopen the app without closing individual session tabs. The same live sessions should reconnect. Check the IDs with `krill ls`.

Closing a session tab or pane explicitly ends that session. App restart continuity depends on the session services remaining alive. Rebooting the machine does not keep the old agent process alive. See [failure guarantees](README.md#what-survives-a-failure).

## First CLI workflow

Run these commands in an external terminal so quitting the GUI does not interrupt the instructions. The first session command starts the local services if needed.

### macOS shell

Change into your repository, then run:

```sh
krill --help
krill agent list
session_id=$(krill new --cwd "$PWD" -- /bin/sh)
krill send "$session_id" --text "printf 'hello from Krill\n'" --enter
krill wait "$session_id" --for pattern --pattern "hello from Krill" --timeout 10
krill read "$session_id" --lines 20
krill ls
```

Success: `new` returns a session ID and `read` includes `hello from Krill`. An agent marked as not installed by `agent list` is optional for this shell check.

### Windows PowerShell

Change into your repository in PowerShell, then run:

```powershell
krill --help
krill agent list
$sessionId = krill new --cwd $PWD.Path -- powershell.exe -NoLogo -NoProfile
krill send $sessionId --text "Write-Output 'hello from Krill'" --enter
krill wait $sessionId --for pattern --pattern "hello from Krill" --timeout 10
krill read $sessionId --lines 20
krill ls
```

Success: a session ID and the greeting appear. These commands use native PowerShell; WSL is not required.

### Reconnect, create a worktree, and launch an agent

Leave the shell running, quit and reopen the GUI, and run `krill ls` again. Read the same ID to confirm continuity. To interact directly, use `krill attach <session_id>`, replacing the placeholder with your actual ID. **Ctrl-]** detaches without terminating the session.

From a repository with a commit, create a separate worktree without running a setup script:

```sh
krill worktree new first-review --setup skip
krill worktree list
```

Choose another name if `first-review` already exists. The creation output prints the new worktree's path and branch. Change into that printed path, then start an installed, authenticated agent:

```sh
krill new --agent claude --prompt "Describe this repository without changing files."
```

Use `--agent codex` for Codex, or a profile shown by `krill agent list`. This starts an actual agent request and uses that agent's account. With no agent configured, repeat the shell workflow in the new directory.

When finished with the example shell, explicitly terminate only its session:

```sh
# macOS: use the session_id variable from above.
krill kill "$session_id"
```

```powershell
# Windows: use the sessionId variable from above.
krill kill $sessionId
```

Run `krill worktree rm <path>` only when you intend to remove that worktree and have reviewed its changes. It is not needed to test reconnection.

## Build from source on macOS

This walkthrough targets **Apple Silicon macOS**. It is not an Intel Mac support promise. Install Git, macOS Command Line Tools, and Rust 1.98 or newer; Python 3 is needed for bundling. Full Xcode and Apple Developer membership are unnecessary for local execution.

In the shared maintainer workspace, Cargo output must stay on the mounted external build volume. Ensure `/Volumes/krill-build` exists and is writable before using the example below; do not create a replacement target on the internal SSD.

```sh
git clone https://github.com/craftowen/krill.git
cd krill
export CARGO_TARGET_DIR=/Volumes/krill-build/targets/krill-dev
cargo build --workspace --bins --locked
export PATH="$CARGO_TARGET_DIR/debug:$PATH"
cargo run -p krill-app --locked
```

Build **all workspace binaries before running the GUI**: the app needs the `krill`, `krill-holder`, and `krilld` helper binaries. The PATH above also makes the CLI examples available in that terminal. The first GUI launch should show the workspace window; follow the GUI steps above. This development run does not install a `Krill.app` bundle.

To assemble a local release app instead:

```sh
packaging/bundle.sh
open packaging/dist/Krill.app
```

The bundler performs its own locked release build and honors Cargo's target location. Its default output must not already exist; choose a fresh `--output <directory>` when repeating. For detailed signing, output, and packaging rules see [packaging](packaging/README.md) (Korean).

The separate `bash packaging/install.sh --source-dir "$PWD"` mode builds and installs the **CLI plus holder/daemon only** under `~/.local/bin`; it does not assemble or install the GUI. Do not confuse this with the default release installer, which installs the app bundle. Windows source-packaging instructions are in [Windows packaging](packaging/windows/README.md) (Korean); the release ZIP is the simpler first-run route.

## If something fails

- **CLI not found:** on macOS add `~/.local/bin` to PATH for a release install, or the configured Cargo debug directory for a development build. On Windows open a new terminal after installation.
- **Agent not detected:** run `krill agent list`; install and authenticate your agent separately, then reopen Krill if PATH changed.
- **Source GUI cannot start services:** build all workspace binaries first and keep matching binaries together.
- **Installation or connection problem:** run `krill doctor`. If sharing `krill doctor --bundle`, review and redact the bundle before attaching it. Record Krill version/commit, OS, architecture, commands, and actual behavior in a [bug report](.github/ISSUE_TEMPLATE/bug_report.md).
- **Possible vulnerability:** follow [Security](SECURITY.md); do not post details in a public bug report.
