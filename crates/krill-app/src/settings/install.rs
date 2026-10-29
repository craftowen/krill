//! 개발 환경·프론티어 CLI 원클릭 설치 및 셸 에일리어스 설정 탭(`Settings > Install`).
//!
//! - 화면 전환 없이 현재 설정 화면 안에서 각 도구(Claude Code, Codex CLI, Antigravity CLI,
//!   Git, Node.js, GitHub CLI, Homebrew/Winget, uv, Bun, pnpm)의 설치 여부·버전을 백그라운드에서 점검하고,
//!   원클릭으로 백그라운드 설치를 실행해 진행 상태·출력 로그를 인라인으로 표시한다.
//! - 대화형 프롬프트나 관리자 권한(`sudo`)이 필요할 때는 언제든 '터미널에서 실행' 보조 버튼으로
//!   Krill 터미널에서 이어서 실행할 수 있다.
//! - AI 프로바이더 CLI(Claude Code, Codex, Antigravity)의 셸 에일리어스(`cc`, `cx`, `ag` 등)를
//!   사용자 셸 설정 파일(`~/.zshrc`, `~/.bashrc`, PowerShell `$PROFILE`)의 관리 블록에 영구 등록하고,
//!   Krill 빠른 명령(`settings.toml`의 `quick_commands`)에도 한 번에 동기화한다.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::time::Duration;

use gpui::{Context, FontWeight, SharedString, Subscription, Window, div, prelude::*, px};
use gpui_component::button::ButtonVariants as _;
use gpui_component::input::{Input, InputState};
use gpui_component::{Disableable as _, Icon, Sizable as _};
use gpui_kit_assets::IconName;

use super::{SettingsView, edit, layout, queue_terminal_command, state_dir};
use crate::i18n::catalog::settings as text;
use crate::ui::BadgeTone;

mod plan;
mod runner;

/// 설치 탭 카테고리.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolCategory {
    FrontierAi,
    CoreDev,
    PackageAndRuntime,
}

/// 설치 가능한 도구 항목 정의.
#[derive(Debug, Clone, Copy)]
pub struct ToolSpec {
    pub id: &'static str,
    pub name: &'static str,
    pub category: ToolCategory,
    /// PATH에서 찾을 실행 파일 후보(첫 번째로 발견된 바이너리로 버전 확인).
    pub bins: &'static [&'static str],
    pub version_args: &'static [&'static str],
    pub install_macos: &'static str,
    pub install_windows: &'static str,
    /// 에이전트 브랜드 아이콘이 있으면 해당 agent id(`"claude"`, `"codex"`, `"agy"`).
    pub agent_icon: Option<&'static str>,
}

impl ToolSpec {
    /// 현재 OS에 맞는 설치 명령어.
    #[must_use]
    pub const fn install_command(&self) -> &'static str {
        if cfg!(windows) {
            self.install_windows
        } else {
            self.install_macos
        }
    }

    /// 도구 설명(한·영 카탈로그).
    #[must_use]
    pub fn description(&self) -> &'static str {
        match self.id {
            "claude" => text::INSTALL_DESC_CLAUDE.get(),
            "codex" => text::INSTALL_DESC_CODEX.get(),
            "agy" => text::INSTALL_DESC_AGY.get(),
            "git" => text::INSTALL_DESC_GIT.get(),
            "node" => text::INSTALL_DESC_NODE.get(),
            "gh" => text::INSTALL_DESC_GH.get(),
            "pkgmgr" => {
                if cfg!(windows) {
                    text::INSTALL_DESC_WINGET.get()
                } else {
                    text::INSTALL_DESC_BREW.get()
                }
            }
            "uv" => text::INSTALL_DESC_UV.get(),
            "bun" => text::INSTALL_DESC_BUN.get(),
            "pnpm" => text::INSTALL_DESC_PNPM.get(),
            _ => "",
        }
    }

    /// 플랫폼별 표시 이름(`pkgmgr`는 macOS에서 Homebrew, Windows에서 Winget).
    #[must_use]
    pub fn display_name(&self) -> &'static str {
        if self.id == "pkgmgr" {
            if cfg!(windows) { "Winget" } else { "Homebrew" }
        } else {
            self.name
        }
    }

    /// 플랫폼별 감지 바이너리 후보.
    #[must_use]
    pub fn probe_bins(&self) -> &'static [&'static str] {
        if self.id == "pkgmgr" {
            if cfg!(windows) {
                &["winget"]
            } else {
                &["brew"]
            }
        } else {
            self.bins
        }
    }
}

/// 전체 설치 카탈로그.
pub const CATALOG: &[ToolSpec] = &[
    // 1. 프론티어 AI 코딩 CLI
    ToolSpec {
        id: "claude",
        name: "Claude Code",
        category: ToolCategory::FrontierAi,
        bins: &["claude"],
        version_args: &["--version"],
        install_macos: "curl -fsSL https://claude.ai/install.sh | bash -s -- latest",
        install_windows: "& ([scriptblock]::Create((irm https://claude.ai/install.ps1))) latest",
        agent_icon: Some("claude"),
    },
    ToolSpec {
        id: "codex",
        name: "Codex CLI",
        category: ToolCategory::FrontierAi,
        bins: &["codex"],
        version_args: &["--version"],
        install_macos: "npm install -g @openai/codex@latest",
        install_windows: "npm.cmd install -g @openai/codex@latest",
        agent_icon: Some("codex"),
    },
    ToolSpec {
        id: "agy",
        name: "Antigravity CLI (agy)",
        category: ToolCategory::FrontierAi,
        bins: &["agy"],
        version_args: &["--version"],
        install_macos: "if command -v agy >/dev/null 2>&1; then agy update; elif [ -x \"$HOME/.local/bin/agy\" ]; then \"$HOME/.local/bin/agy\" update; else curl -fsSL https://antigravity.google/cli/install.sh | bash; fi",
        install_windows: "if (Get-Command agy -ErrorAction SilentlyContinue) { agy update } elseif (Test-Path \"$env:LOCALAPPDATA/agy/bin/agy.exe\") { & \"$env:LOCALAPPDATA/agy/bin/agy.exe\" update } else { irm https://antigravity.google/cli/install.ps1 | iex }",
        agent_icon: Some("agy"),
    },
    // 2. 핵심 개발 도구
    ToolSpec {
        id: "git",
        name: "Git",
        category: ToolCategory::CoreDev,
        bins: &["git"],
        version_args: &["--version"],
        install_macos: "brew install git",
        install_windows: "winget install --id Git.Git -e --source winget --accept-source-agreements --accept-package-agreements --disable-interactivity --silent",
        agent_icon: None,
    },
    ToolSpec {
        id: "node",
        name: "Node.js",
        category: ToolCategory::CoreDev,
        bins: &["node"],
        version_args: &["--version"],
        install_macos: "brew install node",
        install_windows: "winget install --id OpenJS.NodeJS.LTS -e --source winget --accept-source-agreements --accept-package-agreements --disable-interactivity --silent",
        agent_icon: None,
    },
    ToolSpec {
        id: "gh",
        name: "GitHub CLI (gh)",
        category: ToolCategory::CoreDev,
        bins: &["gh"],
        version_args: &["--version"],
        install_macos: "brew install gh",
        install_windows: "winget install --id GitHub.cli -e --source winget --accept-source-agreements --accept-package-agreements --disable-interactivity --silent",
        agent_icon: None,
    },
    // 3. 패키지 매니저 · 모던 런타임
    ToolSpec {
        id: "pkgmgr",
        name: "Homebrew",
        category: ToolCategory::PackageAndRuntime,
        bins: &["brew"],
        version_args: &["--version"],
        install_macos: "krill_brew_install_script=$(curl -fsSL https://raw.githubusercontent.com/Homebrew/install/HEAD/install.sh) && /bin/bash -c \"$krill_brew_install_script\"",
        install_windows: "Start-Process 'https://aka.ms/getwinget'",
        agent_icon: None,
    },
    ToolSpec {
        id: "uv",
        name: "uv (Python)",
        category: ToolCategory::PackageAndRuntime,
        bins: &["uv"],
        version_args: &["--version"],
        install_macos: "curl -LsSf https://astral.sh/uv/install.sh | sh",
        install_windows: "irm https://astral.sh/uv/install.ps1 | iex",
        agent_icon: None,
    },
    ToolSpec {
        id: "bun",
        name: "Bun",
        category: ToolCategory::PackageAndRuntime,
        bins: &["bun"],
        version_args: &["--version"],
        install_macos: "curl -fsSL https://bun.sh/install | bash",
        install_windows: "irm https://bun.sh/install.ps1 | iex",
        agent_icon: None,
    },
    ToolSpec {
        id: "pnpm",
        name: "pnpm",
        category: ToolCategory::PackageAndRuntime,
        bins: &["pnpm"],
        version_args: &["--version"],
        install_macos: "curl -fsSL https://get.pnpm.io/install.sh | sh -",
        install_windows: "npm.cmd install -g pnpm",
        agent_icon: None,
    },
];

/// 도구 하나에 대한 감지 상태.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ToolStatus {
    pub installed: bool,
    pub binary_path: Option<PathBuf>,
    pub version: Option<String>,
    /// 백그라운드에서 감지한 기존 설치 출처에 맞는 명령.
    pub command: Option<String>,
}

/// 인라인 설치 실행 결과.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstallJobResult {
    pub success: bool,
    pub output: String,
}

/// AI 프로바이더 에일리어스 프리셋 정의.
#[derive(Debug, Clone, Copy)]
pub struct AliasPreset {
    pub agent_id: &'static str,
    pub label: &'static str,
    pub default_alias: &'static str,
    pub default_yolo_alias: &'static str,
    pub command: &'static str,
    pub yolo_command: &'static str,
}

pub const ALIAS_PRESETS: &[AliasPreset] = &[
    AliasPreset {
        agent_id: "claude",
        label: "Claude Code",
        default_alias: "cc",
        default_yolo_alias: "ccy",
        command: "claude",
        yolo_command: "claude --dangerously-skip-permissions",
    },
    AliasPreset {
        agent_id: "codex",
        label: "Codex CLI",
        default_alias: "cx",
        default_yolo_alias: "cxy",
        command: "codex",
        yolo_command: "codex --dangerously-bypass-approvals-and-sandbox",
    },
    AliasPreset {
        agent_id: "agy",
        label: "Antigravity CLI",
        default_alias: "ag",
        default_yolo_alias: "agy-yolo",
        command: "agy",
        yolo_command: "agy --dangerously-skip-permissions",
    },
];

pub const ALIAS_BLOCK_START: &str = "# >>> krill ai-aliases >>>";
pub const ALIAS_BLOCK_END: &str = "# <<< krill ai-aliases <<<";

/// 에일리어스 한 줄 설정값.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AliasEntry {
    pub agent_id: &'static str,
    pub alias_name: String,
    pub use_yolo: bool,
}

impl AliasEntry {
    /// 실행할 대상 명령어.
    #[must_use]
    pub fn target_command(&self) -> &'static str {
        let Some(preset) = ALIAS_PRESETS.iter().find(|p| p.agent_id == self.agent_id) else {
            return "";
        };
        if self.use_yolo {
            preset.yolo_command
        } else {
            preset.command
        }
    }
}

struct AliasFormRow {
    alias_input: gpui::Entity<InputState>,
    use_yolo: bool,
    _sub: Subscription,
}

/// `SettingsView`가 소유하는 설치 탭 상태.
#[derive(Default)]
pub struct Panel {
    pub scanned: bool,
    pub scanning: bool,
    pub statuses: BTreeMap<&'static str, ToolStatus>,
    pub installing: BTreeSet<&'static str>,
    pending: VecDeque<&'static str>,
    active: Option<&'static str>,
    pub results: BTreeMap<&'static str, InstallJobResult>,
    pub log_expanded: BTreeSet<&'static str>,
    alias_rows: BTreeMap<&'static str, AliasFormRow>,
    pub alias_sync_quick_commands: bool,
    pub alias_status: Option<(SharedString, bool)>,
    pub alias_configured_path: Option<PathBuf>,
}

impl std::fmt::Debug for Panel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("InstallPanel")
            .field("scanned", &self.scanned)
            .field("scanning", &self.scanning)
            .field("statuses", &self.statuses.len())
            .finish_non_exhaustive()
    }
}

/// 로그인 셸 + 자주 쓰는 사용자 바이너리 경로(`~/.local/bin`, `~/.bun/bin`, `~/.cargo/bin`, Homebrew 등)를 합친 탐색 PATH.
#[must_use]
pub fn effective_search_path(home: &Path) -> std::ffi::OsString {
    let base = crate::onboarding::Env::current()
        .and_then(|env| crate::onboarding::login_path(&env.shell))
        .or_else(|| std::env::var_os("PATH"))
        .unwrap_or_default();
    let mut paths: Vec<PathBuf> = std::env::split_paths(&base).collect();
    let extra = [
        home.join(".local/bin"),
        home.join(".bun/bin"),
        home.join(".cargo/bin"),
        home.join(".npm-global/bin"),
        home.join("Library/pnpm"),
        home.join(".local/share/pnpm"),
        home.join("AppData/Roaming/npm"),
        home.join("AppData/Local/Microsoft/WindowsApps"),
        home.join("AppData/Local/Programs/Antigravity/bin"),
        home.join("AppData/Local/agy/bin"),
        home.join("AppData/Local/pnpm"),
        PathBuf::from("/opt/homebrew/bin"),
        PathBuf::from("/usr/local/bin"),
    ];
    for p in extra {
        if !paths.contains(&p) {
            paths.push(p);
        }
    }
    #[cfg(windows)]
    paths.extend(plan::windows_registry_paths());
    std::env::join_paths(paths).unwrap_or(base)
}

/// 단일 도구의 설치 상태 및 버전을 확인한다(백그라운드 실행기용).
#[must_use]
pub fn probe_tool(spec: &ToolSpec, search_path: &OsStr) -> ToolStatus {
    for &bin in spec.probe_bins() {
        if let Some(exe) = krill_proto::agent::which(bin, search_path) {
            let version = probe_version(&exe, spec.version_args);
            return ToolStatus {
                installed: true,
                binary_path: Some(exe),
                version,
                command: Some(plan::command(spec, search_path)),
            };
        }
    }
    ToolStatus {
        installed: false,
        binary_path: None,
        version: None,
        command: Some(plan::command(spec, search_path)),
    }
}

fn probe_version(exe: &Path, args: &[&str]) -> Option<String> {
    let mut cmd = runner::version_command(exe, args);
    let out = super::child::output(&mut cmd, Duration::from_secs(4), 4096).ok()?;
    if !out.status.success() {
        return None;
    }
    let text = if out.stdout.is_empty() {
        String::from_utf8_lossy(&out.stderr)
    } else {
        String::from_utf8_lossy(&out.stdout)
    };
    let first = text.lines().map(str::trim).find(|l| !l.is_empty())?;
    Some(clean_version_line(first))
}

fn clean_version_line(line: &str) -> String {
    let trimmed = line.trim();
    if trimmed.chars().count() > 48 {
        let short: String = trimmed.chars().take(48).collect();
        format!("{short}…")
    } else {
        trimmed.to_owned()
    }
}

/// 모든 카탈로그 도구를 스캔한다.
#[must_use]
pub fn probe_all_tools(home: &Path) -> BTreeMap<&'static str, ToolStatus> {
    let path = effective_search_path(home);
    CATALOG
        .iter()
        .map(|spec| (spec.id, probe_tool(spec, &path)))
        .collect()
}

fn tail_lines(text: &str, max_lines: usize) -> String {
    let lines: Vec<&str> = text.lines().collect();
    let start = lines.len().saturating_sub(max_lines);
    lines.get(start..).unwrap_or_default().join("\n")
}

/// 에일리어스 이름 유효성 검사(영문·숫자·`_`·`-`만 허용).
#[must_use]
pub fn is_valid_alias_name(name: &str) -> bool {
    let trimmed = name.trim();
    !trimmed.is_empty()
        && trimmed.len() <= 32
        && trimmed
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
        && !trimmed.starts_with('-')
}

/// 현재 플랫폼·셸에 맞는 셸 프로필 파일 경로.
#[must_use]
pub fn shell_rc_path(home: &Path) -> PathBuf {
    if cfg!(windows) {
        home.join("Documents")
            .join("PowerShell")
            .join("Microsoft.PowerShell_profile.ps1")
    } else {
        let shell = std::env::var("SHELL").unwrap_or_default();
        if shell.ends_with("bash") {
            home.join(".bashrc")
        } else {
            home.join(".zshrc")
        }
    }
}

/// 관리 블록 텍스트 생성(Unix 셸 vs PowerShell).
#[must_use]
pub fn render_alias_block(entries: &[AliasEntry], is_powershell: bool) -> String {
    let mut lines = Vec::new();
    lines.push(ALIAS_BLOCK_START.to_owned());
    for entry in entries {
        let name = entry.alias_name.trim();
        if !is_valid_alias_name(name) {
            continue;
        }
        let cmd = entry.target_command();
        if cmd.is_empty() {
            continue;
        }
        if is_powershell {
            lines.push(format!("function {name} {{ {cmd} @args }}"));
        } else {
            lines.push(format!("alias {name}='{cmd}'"));
        }
    }
    lines.push(ALIAS_BLOCK_END.to_owned());
    lines.join("\n")
}

/// 기존 셸 프로필 내용에서 관리 블록을 교체하거나 끝에 추가한다.
#[must_use]
pub fn upsert_alias_block(existing: &str, entries: &[AliasEntry], is_powershell: bool) -> String {
    let block = render_alias_block(entries, is_powershell);
    let cleaned = remove_alias_block(existing);
    let trimmed = cleaned.trim_end();
    if trimmed.is_empty() {
        format!("{block}\n")
    } else {
        format!("{trimmed}\n\n{block}\n")
    }
}

/// 기존 셸 프로필 내용에서 관리 블록을 제거한다.
#[must_use]
pub fn remove_alias_block(existing: &str) -> String {
    let mut out = Vec::new();
    let mut inside = false;
    for line in existing.lines() {
        let trimmed = line.trim();
        if trimmed == ALIAS_BLOCK_START {
            inside = true;
            continue;
        }
        if trimmed == ALIAS_BLOCK_END {
            inside = false;
            continue;
        }
        if !inside {
            out.push(line);
        }
    }
    let joined = out.join("\n");
    let trimmed = joined.trim_end();
    if trimmed.is_empty() {
        String::new()
    } else {
        format!("{trimmed}\n")
    }
}

/// 셸 프로필 파일에서 기존에 저장된 에일리어스 설정을 읽어온다.
#[must_use]
pub fn parse_existing_aliases(existing: &str) -> BTreeMap<String, (String, bool)> {
    let mut map = BTreeMap::new();
    let mut inside = false;
    for line in existing.lines() {
        let trimmed = line.trim();
        if trimmed == ALIAS_BLOCK_START {
            inside = true;
            continue;
        }
        if trimmed == ALIAS_BLOCK_END {
            break;
        }
        if !inside {
            continue;
        }
        // Unix: alias cc='claude --dangerously-skip-permissions'
        if let Some(rest) = trimmed.strip_prefix("alias ")
            && let Some((name, raw_cmd)) = rest.split_once('=')
        {
            let name = name.trim().to_owned();
            let cmd = raw_cmd.trim().trim_matches('\'').trim_matches('"');
            if let Some(preset) = ALIAS_PRESETS
                .iter()
                .find(|p| cmd == p.command || cmd == p.yolo_command)
            {
                map.insert(
                    preset.agent_id.to_owned(),
                    (name, cmd == preset.yolo_command),
                );
            }
        }
        // PowerShell: function cc { claude ... @args }
        if let Some(rest) = trimmed.strip_prefix("function ")
            && let Some((name, body)) = rest.split_once('{')
        {
            let name = name.trim().to_owned();
            let cmd = body
                .trim()
                .trim_end_matches('}')
                .trim()
                .trim_end_matches("@args")
                .trim();
            if let Some(preset) = ALIAS_PRESETS
                .iter()
                .find(|p| cmd == p.command || cmd == p.yolo_command)
            {
                map.insert(
                    preset.agent_id.to_owned(),
                    (name, cmd == preset.yolo_command),
                );
            }
        }
    }
    map
}

impl SettingsView {
    fn install_home_dir(cx: &gpui::App) -> PathBuf {
        if cfg!(test) {
            state_dir(cx).map_or_else(crate::accounts::default_home, |d| d.join("test-home"))
        } else {
            crate::accounts::default_home()
        }
    }

    /// 설치 탭 진입 시 도구 설치 여부를 백그라운드에서 스캔한다.
    pub fn ensure_install_scanned(&mut self, cx: &mut Context<Self>) {
        if self.install.scanned || self.install.scanning {
            return;
        }
        self.rescan_install_tools(cx);
    }

    /// 도구 설치 상태를 백그라운드에서 다시 스캔한다.
    pub fn rescan_install_tools(&mut self, cx: &mut Context<Self>) {
        if self.install.scanning || !self.install.installing.is_empty() {
            return;
        }
        self.install.scanning = true;
        let home = Self::install_home_dir(cx);
        let rc_path = shell_rc_path(&home);
        cx.notify();
        if cfg!(test) {
            self.install.scanning = false;
            self.install.scanned = true;
            for spec in CATALOG {
                self.install.statuses.entry(spec.id).or_default();
            }
            self.install.alias_configured_path = Some(rc_path);
            cx.notify();
            return;
        }
        cx.spawn(async move |this, cx| {
            let statuses = cx
                .background_executor()
                .spawn(async move { probe_all_tools(&home) })
                .await;
            this.update(cx, |this, cx| {
                this.install.scanning = false;
                this.install.scanned = true;
                this.install.statuses = statuses;
                this.install.alias_configured_path = Some(rc_path);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// 단일 도구를 백그라운드에서 인라인 설치하고 완료 후 상태를 갱신한다.
    pub fn run_inline_install(&mut self, tool_id: &'static str, cx: &mut Context<Self>) {
        if self.install.installing.contains(tool_id) {
            return;
        }
        let Some(spec) = CATALOG.iter().find(|s| s.id == tool_id) else {
            return;
        };
        let installed = self
            .install
            .statuses
            .get(tool_id)
            .is_some_and(|s| s.installed);
        if tool_id == "pkgmgr" && (cfg!(windows) || !installed) {
            queue_terminal_command(spec.install_command(), cx);
            return;
        }
        // npm 기반 도구는 Node.js 설치를 먼저 큐에 넣는다. 다음 작업 시작 시 PATH를 다시 읽는다.
        let cmd = self
            .install
            .statuses
            .get(tool_id)
            .and_then(|status| status.command.as_deref())
            .unwrap_or_else(|| spec.install_command());
        if cmd.starts_with("npm")
            && !self
                .install
                .statuses
                .get("node")
                .is_some_and(|s| s.installed)
        {
            self.run_inline_install("node", cx);
        }
        self.install.installing.insert(tool_id);
        self.install.results.remove(tool_id);
        cx.notify();

        if cfg!(test) {
            self.install.installing.remove(tool_id);
            self.install.statuses.insert(
                tool_id,
                ToolStatus {
                    installed: true,
                    binary_path: Some(PathBuf::from(
                        spec.probe_bins().first().copied().unwrap_or(tool_id),
                    )),
                    version: Some("1.0.0-test".to_owned()),
                    command: Some(spec.install_command().to_owned()),
                },
            );
            self.install.results.insert(
                tool_id,
                InstallJobResult {
                    success: true,
                    output: format!("installed {tool_id}"),
                },
            );
            cx.notify();
            return;
        }

        // 설치 관리자는 공유 의존성을 잠그므로 도구 종류에 관계없이 순서대로 실행한다.
        self.install.pending.push_back(tool_id);
        self.start_next_install(cx);
    }

    fn start_next_install(&mut self, cx: &mut Context<Self>) {
        if self.install.active.is_some() {
            return;
        }
        let Some(tool_id) = self.install.pending.pop_front() else {
            return;
        };
        let Some(spec) = CATALOG.iter().find(|s| s.id == tool_id) else {
            return;
        };
        self.install.active = Some(tool_id);
        let home = Self::install_home_dir(cx);
        cx.notify();
        cx.spawn(async move |this, cx| {
            let (res, statuses) = cx
                .background_executor()
                .spawn(async move {
                    let path = effective_search_path(&home);
                    let command = plan::command(spec, &path);
                    let res = if let Some(message) = plan::prerequisite(spec, &path) {
                        InstallJobResult {
                            success: false,
                            output: message,
                        }
                    } else {
                        runner::run(&command, &home, &path)
                    };
                    let statuses = probe_all_tools(&home);
                    let mut res = res;
                    if res.success
                        && !statuses
                            .get(tool_id)
                            .is_some_and(|s| s.installed && s.version.is_some())
                    {
                        res.success = false;
                        res.output.push('\n');
                        res.output.push_str(text::INSTALL_VERIFY_FAILED.get());
                    }
                    (res, statuses)
                })
                .await;
            this.update(cx, |this, cx| {
                this.install.active = None;
                this.install.installing.remove(tool_id);
                if !res.success {
                    this.install.log_expanded.insert(tool_id);
                }
                this.install.results.insert(tool_id, res);
                this.install.statuses = statuses;
                this.start_next_install(cx);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// 미설치된 프론티어 AI CLI들을 한 번에 백그라운드 설치한다.
    pub fn install_missing_frontier_clis(&mut self, cx: &mut Context<Self>) {
        let missing: Vec<&'static str> = CATALOG
            .iter()
            .filter(|s| s.category == ToolCategory::FrontierAi)
            .filter(|s| {
                !self
                    .install
                    .statuses
                    .get(s.id)
                    .is_some_and(|st| st.installed)
            })
            .map(|s| s.id)
            .collect();
        for id in missing {
            self.run_inline_install(id, cx);
        }
    }

    fn ensure_alias_form(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.install.alias_rows.is_empty() {
            return;
        }
        let home = Self::install_home_dir(cx);
        let rc_path = shell_rc_path(&home);
        let existing_content = std::fs::read_to_string(&rc_path).unwrap_or_default();
        let parsed = parse_existing_aliases(&existing_content);
        self.install.alias_configured_path = Some(rc_path);
        self.install.alias_sync_quick_commands = true;

        for preset in ALIAS_PRESETS {
            let (default_val, use_yolo) = parsed
                .get(preset.agent_id)
                .cloned()
                .unwrap_or_else(|| (preset.default_alias.to_owned(), true));
            let input = cx.new(|cx| {
                InputState::new(window, cx)
                    .placeholder(preset.default_alias)
                    .default_value(default_val)
            });
            let sub = cx.subscribe_in(
                &input,
                window,
                |this, _, ev: &gpui_component::input::InputEvent, _, cx| {
                    if matches!(ev, gpui_component::input::InputEvent::PressEnter { .. }) {
                        this.save_shell_aliases(cx);
                    }
                },
            );
            self.install.alias_rows.insert(
                preset.agent_id,
                AliasFormRow {
                    alias_input: input,
                    use_yolo,
                    _sub: sub,
                },
            );
        }
    }

    /// 에일리어스 설정을 셸 설정 파일(`~/.zshrc` 등)과 빠른 명령(`quick_commands`)에 저장한다.
    pub fn save_shell_aliases(&mut self, cx: &mut Context<Self>) {
        let mut entries = Vec::new();
        for preset in ALIAS_PRESETS {
            let Some(row) = self.install.alias_rows.get(preset.agent_id) else {
                continue;
            };
            let raw = row.alias_input.read(cx).value().trim().to_owned();
            if raw.is_empty() {
                continue;
            }
            if !is_valid_alias_name(&raw) {
                self.install.alias_status = Some((
                    text::INSTALL_ALIAS_ERR_INVALID
                        .format(&[("alias", &raw)])
                        .into(),
                    true,
                ));
                cx.notify();
                return;
            }
            entries.push(AliasEntry {
                agent_id: preset.agent_id,
                alias_name: raw,
                use_yolo: row.use_yolo,
            });
        }

        let home = Self::install_home_dir(cx);
        let rc_path = shell_rc_path(&home);
        let is_ps = cfg!(windows);
        let existing = std::fs::read_to_string(&rc_path).unwrap_or_default();
        let updated = upsert_alias_block(&existing, &entries, is_ps);

        if let Some(parent) = rc_path.parent()
            && let Err(e) = std::fs::create_dir_all(parent)
        {
            self.install.alias_status = Some((e.to_string().into(), true));
            cx.notify();
            return;
        }
        if let Err(e) = std::fs::write(&rc_path, updated) {
            self.install.alias_status = Some((e.to_string().into(), true));
            cx.notify();
            return;
        }

        if self.install.alias_sync_quick_commands {
            let entries_clone = entries.clone();
            edit(cx, move |s| {
                for entry in &entries_clone {
                    let cmd_text = entry.target_command().to_owned();
                    if let Some(existing_qc) = s
                        .quick_commands
                        .iter_mut()
                        .find(|q| q.name == entry.alias_name)
                    {
                        existing_qc.text = cmd_text;
                    } else {
                        s.quick_commands.push(crate::commands::QuickCommand {
                            name: entry.alias_name.clone(),
                            text: cmd_text,
                        });
                    }
                }
            });
        }

        self.install.alias_status = Some((
            text::INSTALL_ALIAS_SAVED
                .format(&[("path", &rc_path.display().to_string())])
                .into(),
            false,
        ));
        cx.notify();
    }

    /// 셸 설정 파일에서 Krill이 등록한 에일리어스 블록을 제거한다.
    pub fn remove_shell_aliases(&mut self, cx: &mut Context<Self>) {
        let home = Self::install_home_dir(cx);
        let rc_path = shell_rc_path(&home);
        let Ok(existing) = std::fs::read_to_string(&rc_path) else {
            self.install.alias_status = Some((text::INSTALL_ALIAS_REMOVED.get().into(), false));
            cx.notify();
            return;
        };
        let cleaned = remove_alias_block(&existing);
        if let Err(e) = std::fs::write(&rc_path, cleaned) {
            self.install.alias_status = Some((e.to_string().into(), true));
        } else {
            self.install.alias_status = Some((text::INSTALL_ALIAS_REMOVED.get().into(), false));
        }
        cx.notify();
    }

    /// 설치 탭 전체 렌더링.
    pub(super) fn render_install_tab(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> gpui::Div {
        self.ensure_install_scanned(cx);
        self.ensure_alias_form(window, cx);

        let p = crate::ui::palette(cx);
        let installed_count = CATALOG
            .iter()
            .filter(|s| {
                self.install
                    .statuses
                    .get(s.id)
                    .is_some_and(|st| st.installed)
            })
            .count();
        let total_count = CATALOG.len();
        let missing_frontier = CATALOG
            .iter()
            .filter(|s| s.category == ToolCategory::FrontierAi)
            .filter(|s| {
                !self
                    .install
                    .statuses
                    .get(s.id)
                    .is_some_and(|st| st.installed)
            })
            .count();
        let any_installing = !self.install.installing.is_empty();

        // 상단 요약 & 일괄 작업 카드
        let summary_text = text::INSTALL_SUMMARY_COUNT.format(&[
            ("installed", &installed_count.to_string()),
            ("total", &total_count.to_string()),
        ]);
        let os_badge = if cfg!(windows) {
            "Windows (PowerShell / winget)"
        } else {
            "macOS (zsh / bash / Homebrew)"
        };

        let summary_card = layout::card(p)
            .id("admin-install-summary-card")
            .debug_selector(|| "admin-install-summary-card".into())
            .flex()
            .flex_col()
            .gap_2p5()
            .child(
                div()
                    .w_full()
                    .min_w_0()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .justify_between()
                    .gap_3()
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap_2()
                                    .child(
                                        div()
                                            .text_size(crate::ui::TEXT_SM)
                                            .font_weight(FontWeight::SEMIBOLD)
                                            .text_color(p.foreground)
                                            .child(summary_text),
                                    )
                                    .child(crate::ui::badge(p, BadgeTone::Muted, os_badge)),
                            )
                            .child(
                                div()
                                    .text_size(crate::ui::TEXT_XS)
                                    .text_color(p.muted_foreground)
                                    .child(text::INSTALL_SUMMARY_DESC.get()),
                            ),
                    )
                    .child(
                        layout::controls()
                            .child(
                                layout::secondary(
                                    "admin-install-rescan",
                                    if self.install.scanning {
                                        text::INSTALL_SCANNING.get()
                                    } else {
                                        text::INSTALL_RESCAN.get()
                                    },
                                )
                                .icon(IconName::RefreshCw)
                                .disabled(self.install.scanning || any_installing)
                                .debug_selector(|| "admin-install-rescan".into())
                                .on_click(cx.listener(
                                    |this, _, _, cx| {
                                        this.rescan_install_tools(cx);
                                    },
                                )),
                            )
                            .when(missing_frontier > 0, |d| {
                                d.child(
                                    layout::primary(
                                        "admin-install-all-frontier",
                                        text::INSTALL_ALL_FRONTIER.get(),
                                    )
                                    .disabled(any_installing)
                                    .debug_selector(|| "admin-install-all-frontier".into())
                                    .on_click(cx.listener(
                                        |this, _, _, cx| {
                                            this.install_missing_frontier_clis(cx);
                                        },
                                    )),
                                )
                            }),
                    ),
            );

        let frontier_card = self.render_tool_category_card(
            "admin-install-cat-frontier",
            text::INSTALL_CAT_FRONTIER.get(),
            text::INSTALL_CAT_FRONTIER_DESC.get(),
            ToolCategory::FrontierAi,
            cx,
        );
        let core_card = self.render_tool_category_card(
            "admin-install-cat-core",
            text::INSTALL_CAT_CORE.get(),
            text::INSTALL_CAT_CORE_DESC.get(),
            ToolCategory::CoreDev,
            cx,
        );
        let runtime_card = self.render_tool_category_card(
            "admin-install-cat-runtime",
            text::INSTALL_CAT_RUNTIME.get(),
            text::INSTALL_CAT_RUNTIME_DESC.get(),
            ToolCategory::PackageAndRuntime,
            cx,
        );
        let alias_card = self.render_alias_card(cx);

        div()
            .flex()
            .flex_col()
            .gap_4()
            .child(summary_card)
            .child(frontier_card)
            .child(alias_card)
            .child(core_card)
            .child(runtime_card)
    }

    fn render_tool_category_card(
        &self,
        card_id: &'static str,
        title: &'static str,
        desc: &'static str,
        category: ToolCategory,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let p = crate::ui::palette(cx);
        let mut card = layout::card(p)
            .id(card_id)
            .debug_selector(move || card_id.into())
            .flex()
            .flex_col()
            .gap_0()
            .child(
                div()
                    .w_full()
                    .min_w_0()
                    .pb_2p5()
                    .border_b_1()
                    .border_color(p.border)
                    .child(layout::label(p, title, Some(desc))),
            );

        let specs: Vec<&ToolSpec> = CATALOG.iter().filter(|s| s.category == category).collect();
        for (idx, spec) in specs.into_iter().enumerate() {
            card = card.child(self.render_tool_row(idx, spec, cx));
        }
        card
    }

    fn render_tool_row(
        &self,
        idx: usize,
        spec: &'static ToolSpec,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let p = crate::ui::palette(cx);
        let tool_id = spec.id;
        let status = self.install.statuses.get(tool_id);
        let is_installed = status.is_some_and(|s| s.installed);
        let is_installing = self.install.installing.contains(tool_id);
        let is_queued = is_installing && self.install.active != Some(tool_id);
        let job_result = self.install.results.get(tool_id);
        let log_open = self.install.log_expanded.contains(tool_id);

        let (badge_tone, badge_text) = if is_installing {
            (
                BadgeTone::Warning,
                if is_queued {
                    text::INSTALL_STATUS_QUEUED.get()
                } else {
                    text::INSTALL_STATUS_INSTALLING.get()
                }
                .to_owned(),
            )
        } else if is_installed {
            let ver = status
                .and_then(|s| s.version.as_deref())
                .filter(|v| !v.is_empty());
            match ver {
                Some(v) => (
                    BadgeTone::Success,
                    format!("{} · {v}", text::INSTALL_STATUS_INSTALLED.get()),
                ),
                None => (
                    BadgeTone::Success,
                    text::INSTALL_STATUS_INSTALLED.get().to_owned(),
                ),
            }
        } else {
            (
                BadgeTone::Muted,
                text::CONNECTION_NOT_INSTALLED.get().to_owned(),
            )
        };

        let icon_el = if let Some(agent_id) = spec.agent_icon {
            crate::ui::icons::agent_brand_icon(agent_id, crate::ui::ICON_XL, p.foreground)
                .into_any_element()
        } else {
            Icon::new(IconName::SquareTerminal)
                .small()
                .text_color(p.foreground)
                .into_any_element()
        };

        let requires_terminal = tool_id == "pkgmgr" && (cfg!(windows) || !is_installed);
        let cmd_str = status
            .and_then(|s| s.command.as_deref())
            .unwrap_or_else(|| spec.install_command())
            .to_owned();
        let row_id = format!("admin-install-row-{tool_id}");
        let badge_id = format!("admin-install-badge-{tool_id}");
        let btn_install_id = format!("admin-install-btn-{tool_id}");
        let btn_term_id = format!("admin-install-term-{tool_id}");
        let btn_copy_id = format!("admin-install-copy-{tool_id}");
        let btn_log_id = format!("admin-install-log-toggle-{tool_id}");

        let primary_btn_label = if is_installing {
            if is_queued {
                text::INSTALL_STATUS_QUEUED.get()
            } else {
                text::INSTALL_BTN_INSTALLING.get()
            }
        } else if requires_terminal {
            text::INSTALL_IN_TERMINAL.get()
        } else if is_installed {
            text::INSTALL_BTN_REINSTALL.get()
        } else {
            text::INSTALL_BTN_INSTALL.get()
        };

        let install_btn = if is_installed {
            layout::secondary(
                gpui::ElementId::Name(btn_install_id.clone().into()),
                primary_btn_label,
            )
        } else {
            layout::primary(
                gpui::ElementId::Name(btn_install_id.clone().into()),
                primary_btn_label,
            )
        }
        .disabled(
            self.install.scanning
                || is_installing
                || (requires_terminal && !self.install.installing.is_empty()),
        )
        .debug_selector(move || btn_install_id.clone())
        .on_click(cx.listener(move |this, _, _, cx| {
            this.run_inline_install(tool_id, cx);
        }));

        let cmd_for_term = cmd_str.clone();
        let term_btn = layout::ghost(
            gpui::ElementId::Name(btn_term_id.clone().into()),
            text::INSTALL_IN_TERMINAL.get(),
        )
        .disabled(self.install.scanning || !self.install.installing.is_empty())
        .debug_selector(move || btn_term_id.clone())
        .on_click(move |_, _, cx| {
            cx.write_to_clipboard(gpui::ClipboardItem::new_string(cmd_for_term.clone()));
            queue_terminal_command(cmd_for_term.clone(), cx);
        });

        let cmd_for_copy = cmd_str.clone();
        let copy_btn = crate::ui::button_with_tooltip(
            gpui_component::button::Button::new(gpui::ElementId::Name(btn_copy_id.clone().into()))
                .icon(IconName::Copy)
                .ghost()
                .xsmall()
                .debug_selector(move || btn_copy_id.clone())
                .on_click(move |_, _, cx| {
                    cx.write_to_clipboard(gpui::ClipboardItem::new_string(cmd_for_copy.clone()));
                }),
            format!("admin-install-copy-tip-{tool_id}"),
            text::INSTALL_COPY_CMD.get(),
        );

        let mut row = div()
            .id(gpui::ElementId::Name(row_id.clone().into()))
            .debug_selector(move || row_id.clone())
            .w_full()
            .min_w_0()
            .py_3()
            .flex()
            .flex_col()
            .gap_2()
            .when(idx > 0, |d| d.border_t_1().border_color(p.border))
            .child(
                div()
                    .w_full()
                    .min_w_0()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_3()
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .items_center()
                            .gap_2p5()
                            .child(
                                div()
                                    .flex_none()
                                    .size(px(28.))
                                    .rounded(crate::ui::RADIUS_SM)
                                    .bg(p.muted)
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .child(icon_el),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .flex()
                                    .flex_col()
                                    .gap_0p5()
                                    .child(
                                        div()
                                            .min_w_0()
                                            .flex()
                                            .items_center()
                                            .gap_1p5()
                                            .child(
                                                div()
                                                    .min_w_0()
                                                    .truncate()
                                                    .text_size(crate::ui::TEXT_SM)
                                                    .font_weight(FontWeight::SEMIBOLD)
                                                    .text_color(p.foreground)
                                                    .child(spec.display_name()),
                                            )
                                            .child(
                                                crate::ui::badge(p, badge_tone, badge_text)
                                                    .id(gpui::ElementId::Name(
                                                        badge_id.clone().into(),
                                                    ))
                                                    .debug_selector(move || badge_id.clone()),
                                            ),
                                    )
                                    .child(
                                        div()
                                            .min_w_0()
                                            .text_size(crate::ui::TEXT_XS)
                                            .text_color(p.muted_foreground)
                                            .child(spec.description()),
                                    )
                                    .child(
                                        div()
                                            .min_w_0()
                                            .flex()
                                            .items_center()
                                            .gap_1()
                                            .child(
                                                div()
                                                    .min_w_0()
                                                    .truncate()
                                                    .font_family(crate::ui::FONT_MONO)
                                                    .text_size(crate::ui::TEXT_2XS)
                                                    .text_color(p.muted_foreground)
                                                    .child(cmd_str),
                                            )
                                            .child(copy_btn),
                                    ),
                            ),
                    )
                    .child(
                        layout::controls()
                            .child(install_btn)
                            .when(!requires_terminal, |d| d.child(term_btn))
                            .when_some(job_result, |d, res| {
                                let label = if res.success {
                                    text::INSTALL_LOG_DONE.get()
                                } else {
                                    text::INSTALL_LOG_FAILED.get()
                                };
                                d.child(
                                    layout::ghost(
                                        gpui::ElementId::Name(btn_log_id.clone().into()),
                                        label,
                                    )
                                    .debug_selector(move || btn_log_id.clone())
                                    .on_click(cx.listener(
                                        move |this, _, _, cx| {
                                            if !this.install.log_expanded.remove(tool_id) {
                                                this.install.log_expanded.insert(tool_id);
                                            }
                                            cx.notify();
                                        },
                                    )),
                                )
                            }),
                    ),
            );

        if let Some(res) = job_result
            && (log_open || !res.success)
        {
            let log_box_id = format!("admin-install-log-{tool_id}");
            row = row.child(
                div()
                    .id(gpui::ElementId::Name(log_box_id.clone().into()))
                    .debug_selector(move || log_box_id.clone())
                    .w_full()
                    .min_w_0()
                    .p_2()
                    .rounded(crate::ui::RADIUS_SM)
                    .bg(p.background)
                    .border_1()
                    .border_color(if res.success {
                        p.border
                    } else {
                        p.destructive.opacity(0.4)
                    })
                    .font_family(crate::ui::FONT_MONO)
                    .text_size(crate::ui::TEXT_XS)
                    .text_color(if res.success {
                        p.muted_foreground
                    } else {
                        p.destructive
                    })
                    .child(res.output.clone()),
            );
        }

        row
    }

    fn render_alias_card(&self, cx: &mut Context<Self>) -> gpui::Stateful<gpui::Div> {
        let p = crate::ui::palette(cx);
        let rc_display = self
            .install
            .alias_configured_path
            .as_ref()
            .map_or_else(|| "~/.zshrc".to_owned(), |path| path.display().to_string());

        let mut card = layout::card(p)
            .id("admin-install-alias-card")
            .debug_selector(|| "admin-install-alias-card".into())
            .flex()
            .flex_col()
            .gap_3()
            .child(
                div()
                    .w_full()
                    .min_w_0()
                    .pb_2p5()
                    .border_b_1()
                    .border_color(p.border)
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .justify_between()
                    .gap_3()
                    .child(layout::label(
                        p,
                        text::INSTALL_ALIAS_TITLE.get(),
                        Some(text::INSTALL_ALIAS_DESC.get()),
                    ))
                    .child(
                        div()
                            .font_family(crate::ui::FONT_MONO)
                            .text_size(crate::ui::TEXT_2XS)
                            .text_color(p.muted_foreground)
                            .px_2()
                            .py_0p5()
                            .rounded(crate::ui::RADIUS_XS)
                            .bg(p.muted)
                            .child(rc_display),
                    ),
            );

        for preset in ALIAS_PRESETS {
            let agent_id = preset.agent_id;
            let Some(row_state) = self.install.alias_rows.get(agent_id) else {
                continue;
            };
            let input_entity = row_state.alias_input.clone();
            let use_yolo = row_state.use_yolo;
            let preview_cmd = if use_yolo {
                preset.yolo_command
            } else {
                preset.command
            };
            let alias_val = input_entity.read(cx).value().trim().to_owned();
            let preview_line = if alias_val.is_empty() {
                format!("# {}", text::INSTALL_ALIAS_DISABLED.get())
            } else if cfg!(windows) {
                format!("function {alias_val} {{ {preview_cmd} @args }}")
            } else {
                format!("alias {alias_val}='{preview_cmd}'")
            };

            let row_id = format!("admin-install-alias-row-{agent_id}");
            let input_id = format!("admin-install-alias-input-{agent_id}");
            let yolo_id = format!("admin-install-alias-yolo-{agent_id}");
            let normal_id = format!("admin-install-alias-normal-{agent_id}");

            let seg_yolo = layout::segment(
                p,
                gpui::ElementId::Name(yolo_id.clone().into()),
                text::INSTALL_ALIAS_MODE_YOLO.get(),
                use_yolo,
            )
            .debug_selector(move || yolo_id.clone())
            .on_click(cx.listener(move |this, _, _, cx| {
                if let Some(r) = this.install.alias_rows.get_mut(agent_id) {
                    r.use_yolo = true;
                    cx.notify();
                }
            }));

            let seg_normal = layout::segment(
                p,
                gpui::ElementId::Name(normal_id.clone().into()),
                text::INSTALL_ALIAS_MODE_NORMAL.get(),
                !use_yolo,
            )
            .debug_selector(move || normal_id.clone())
            .on_click(cx.listener(move |this, _, _, cx| {
                if let Some(r) = this.install.alias_rows.get_mut(agent_id) {
                    r.use_yolo = false;
                    cx.notify();
                }
            }));

            card = card.child(
                div()
                    .id(gpui::ElementId::Name(row_id.clone().into()))
                    .debug_selector(move || row_id.clone())
                    .w_full()
                    .min_w_0()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .justify_between()
                    .gap_3()
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .items_center()
                            .gap_2p5()
                            .child(
                                div()
                                    .flex_none()
                                    .size(px(28.))
                                    .rounded(crate::ui::RADIUS_SM)
                                    .bg(p.muted)
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .child(crate::ui::icons::agent_brand_icon(
                                        agent_id,
                                        crate::ui::ICON_XL,
                                        p.foreground,
                                    )),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .flex()
                                    .flex_col()
                                    .gap_0p5()
                                    .child(
                                        div()
                                            .text_size(crate::ui::TEXT_SM)
                                            .font_weight(FontWeight::SEMIBOLD)
                                            .text_color(p.foreground)
                                            .child(preset.label),
                                    )
                                    .child(
                                        div()
                                            .min_w_0()
                                            .truncate()
                                            .font_family(crate::ui::FONT_MONO)
                                            .text_size(crate::ui::TEXT_2XS)
                                            .text_color(p.muted_foreground)
                                            .child(preview_line),
                                    ),
                            ),
                    )
                    .child(
                        layout::controls()
                            .child(
                                div()
                                    .id(gpui::ElementId::Name(input_id.clone().into()))
                                    .debug_selector(move || input_id.clone())
                                    .w(px(110.))
                                    .flex_none()
                                    .child(Input::new(&input_entity).small()),
                            )
                            .child(layout::segmented_control(p, [seg_yolo, seg_normal])),
                    ),
            );
        }

        // 빠른 명령 연동 스위치 & 저장 버튼
        let sync_qc = self.install.alias_sync_quick_commands;
        let view = cx.entity();
        card = card.child(
            div()
                .w_full()
                .min_w_0()
                .pt_2p5()
                .border_t_1()
                .border_color(p.border)
                .flex()
                .flex_wrap()
                .items_center()
                .justify_between()
                .gap_3()
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(layout::switch(
                            p,
                            "admin-install-alias-sync-qc",
                            sync_qc,
                            false,
                            move |next, _, cx| {
                                view.update(cx, |this, cx| {
                                    this.install.alias_sync_quick_commands = next;
                                    cx.notify();
                                });
                            },
                        ))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .text_size(crate::ui::TEXT_XS)
                                .text_color(p.muted_foreground)
                                .child(text::INSTALL_ALIAS_SYNC_QC.get()),
                        ),
                )
                .child(
                    layout::controls()
                        .child(
                            layout::ghost(
                                "admin-install-alias-remove",
                                text::INSTALL_ALIAS_REMOVE_BTN.get(),
                            )
                            .debug_selector(|| "admin-install-alias-remove".into())
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.remove_shell_aliases(cx);
                            })),
                        )
                        .child(
                            layout::primary(
                                "admin-install-alias-save",
                                text::INSTALL_ALIAS_SAVE_BTN.get(),
                            )
                            .debug_selector(|| "admin-install-alias-save".into())
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.save_shell_aliases(cx);
                            })),
                        ),
                ),
        );

        if let Some((msg, is_err)) = &self.install.alias_status {
            card = card.child(
                div()
                    .id("admin-install-alias-status")
                    .debug_selector(|| "admin-install-alias-status".into())
                    .w_full()
                    .min_w_0()
                    .text_size(crate::ui::TEXT_XS)
                    .text_color(if *is_err { p.destructive } else { p.success })
                    .child(msg.clone()),
            );
        }

        card
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn alias_block_upsert_and_remove_preserves_surrounding_rc_lines() {
        let entries = vec![
            AliasEntry {
                agent_id: "claude",
                alias_name: "cc".into(),
                use_yolo: true,
            },
            AliasEntry {
                agent_id: "codex",
                alias_name: "cx".into(),
                use_yolo: false,
            },
            AliasEntry {
                agent_id: "agy",
                alias_name: "ag".into(),
                use_yolo: true,
            },
        ];

        let initial = "export PATH=\"$HOME/.local/bin:$PATH\"\n";
        let updated = upsert_alias_block(initial, &entries, false);
        assert!(updated.contains("export PATH=\"$HOME/.local/bin:$PATH\""));
        assert!(updated.contains(ALIAS_BLOCK_START));
        assert!(updated.contains("alias cc='claude --dangerously-skip-permissions'"));
        assert!(updated.contains("alias cx='codex'"));
        assert!(updated.contains("alias ag='agy --dangerously-skip-permissions'"));
        assert!(updated.contains(ALIAS_BLOCK_END));

        let parsed = parse_existing_aliases(&updated);
        assert_eq!(parsed.get("claude"), Some(&("cc".to_owned(), true)));
        assert_eq!(parsed.get("codex"), Some(&("cx".to_owned(), false)));
        assert_eq!(parsed.get("agy"), Some(&("ag".to_owned(), true)));

        // Replace again with modified alias
        let entries2 = vec![AliasEntry {
            agent_id: "claude",
            alias_name: "cl".into(),
            use_yolo: false,
        }];
        let updated2 = upsert_alias_block(&updated, &entries2, false);
        assert_eq!(updated2.matches(ALIAS_BLOCK_START).count(), 1);
        assert!(updated2.contains("alias cl='claude'"));
        assert!(!updated2.contains("alias cc="));

        // Remove block restores original content
        let removed = remove_alias_block(&updated2);
        assert_eq!(removed, initial);
    }

    #[test]
    fn alias_block_without_end_marker_keeps_following_rc_lines() {
        let entries = vec![AliasEntry {
            agent_id: "claude",
            alias_name: "cc".into(),
            use_yolo: false,
        }];
        let rc = format!("export A=1\n{ALIAS_BLOCK_START}\nalias cc='claude'\nexport B=2\n");
        for out in [
            remove_alias_block(&rc),
            upsert_alias_block(&rc, &entries, false),
        ] {
            assert!(out.contains("export A=1"), "{out}");
            assert!(out.contains("export B=2"), "{out}");
        }
    }

    #[test]
    fn powershell_alias_block_rendering_and_parsing() {
        let entries = vec![AliasEntry {
            agent_id: "claude",
            alias_name: "cc".into(),
            use_yolo: true,
        }];
        let block = upsert_alias_block("", &entries, true);
        assert!(block.contains("function cc { claude --dangerously-skip-permissions @args }"));
        let parsed = parse_existing_aliases(&block);
        assert_eq!(parsed.get("claude"), Some(&("cc".to_owned(), true)));
    }

    #[test]
    fn alias_name_validation() {
        assert!(is_valid_alias_name("cc"));
        assert!(is_valid_alias_name("agy-yolo_1"));
        assert!(!is_valid_alias_name(""));
        assert!(!is_valid_alias_name("-bad"));
        assert!(!is_valid_alias_name("bad space"));
        assert!(!is_valid_alias_name("bad;rm"));
    }

    #[test]
    fn frontier_catalog_matches_latest_builtin_profiles_on_both_platforms() {
        let agents = krill_proto::agent::Agents::load(None).unwrap();
        for spec in CATALOG
            .iter()
            .filter(|s| s.category == ToolCategory::FrontierAi)
        {
            let profile = &agents.profiles[spec.id];
            assert_eq!(spec.install_macos, profile.install, "{} macOS", spec.id);
            assert_eq!(
                spec.install_windows, profile.install_windows,
                "{} Windows",
                spec.id
            );
        }
    }

    #[test]
    fn catalog_has_non_empty_platform_commands() {
        for spec in CATALOG {
            assert!(!spec.install_macos.is_empty(), "{} macos cmd", spec.id);
            assert!(!spec.install_windows.is_empty(), "{} windows cmd", spec.id);
            assert!(
                !spec.install_command().is_empty(),
                "{} current cmd",
                spec.id
            );
            assert!(!spec.display_name().is_empty());
            assert!(!spec.probe_bins().is_empty());
        }
    }
}
