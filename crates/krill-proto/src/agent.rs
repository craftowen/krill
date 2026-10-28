//! 에이전트 프로필(MK-037): 내장 5종 + 사용자 `agents.toml`, PATH 감지·버전.
//!
//! 내장 프로필도 사용자 파일과 같은 TOML([`BUILTIN`])이라 사용자가 같은 id로 덮어쓰거나 새 id를 더한다.
//! 새 세션의 기본 권한은 YOLO이고, `permission = "manual"`로 에이전트 자체 권한을 쓸 수 있다.
//! `yolo_args`는 전역 `permission = "yolo"`일 때만 붙는다
//! (ARCHITECTURE §6, T1-D6). 모르는 키는 무시한다(hook·session 등은 뒤 이슈가 더한다).

use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

use serde::{Deserialize, Serialize};

pub mod claude;
pub mod external;
pub mod hooks;
pub mod rules;

/// 내장 5종(T4 §3.1.3 + Antigravity CLI). YOLO 인자: claude·codex·agy는 이 맥의 `--help`로 확인,
/// cursor-agent `--force`는 미설치라 미확인. `OpenCode` `--auto`는 설치된 CLI의 `--help`로 확인.
pub const BUILTIN: &str = r#"
[agent.claude]
name = "Claude Code"
launch = ["claude"]
yolo_args = ["--dangerously-skip-permissions"]
install = "curl -fsSL https://claude.ai/install.sh | bash -s -- latest"
install_windows = '& ([scriptblock]::Create((irm https://claude.ai/install.ps1))) latest'

[agent.codex]
name = "Codex"
launch = ["codex"]
yolo_args = ["--dangerously-bypass-approvals-and-sandbox"]
install = "npm install -g @openai/codex@latest"
install_windows = "npm.cmd install -g @openai/codex@latest"

[agent.opencode]
name = "OpenCode"
launch = ["opencode"]
yolo_args = ["--auto"]
install = "curl -fsSL https://opencode.ai/install | bash"
install_windows = "npm install -g opencode-ai"

[agent.cursor]
name = "Cursor CLI"
launch = ["cursor-agent"]
yolo_args = ["--force"]
install = "curl https://cursor.com/install -fsS | bash"
install_windows = "irm 'https://cursor.com/install?win32=true' | iex"

[agent.agy]
name = "Antigravity"
launch = ["agy"]
yolo_args = ["--dangerously-skip-permissions"]
install = 'if command -v agy >/dev/null 2>&1; then agy update; elif [ -x "$HOME/.local/bin/agy" ]; then "$HOME/.local/bin/agy" update; else curl -fsSL https://antigravity.google/cli/install.sh | bash; fi'
install_windows = 'if (Get-Command agy -ErrorAction SilentlyContinue) { agy update } elseif (Test-Path "$env:LOCALAPPDATA/agy/bin/agy.exe") { & "$env:LOCALAPPDATA/agy/bin/agy.exe" update } else { irm https://antigravity.google/cli/install.ps1 | iex }'
"#;

/// 추가 에이전트 생태계 프리셋(MK-229). `Agents::load(None)`의 기본 5종([`BUILTIN`])은 그대로 두고,
/// `Agents::resolve(id)` 및 `krill agent list`에서 별도 설정 없이 바로 해석·실행할 수 있게 한다.
pub const PRESETS: &str = r#"
[agent.aider]
name = "Aider"
launch = ["aider"]
yolo_args = ["--yes-always"]
install = "pipx install aider-chat"

[agent.goose]
name = "Goose"
launch = ["goose"]
install = "curl -fsSL https://github.com/block/goose/releases/download/stable/download_cli.sh | bash"
install_windows = "irm https://raw.githubusercontent.com/block/goose/main/download_cli.ps1 | iex"

[agent.copilot]
name = "GitHub Copilot CLI"
launch = ["github-copilot-cli"]
install = "npm install -g @githubnext/github-copilot-cli"

[agent.amp]
name = "Amp"
launch = ["amp"]
install = "npm install -g @sourcegraph/amp"

[agent.pi]
name = "Pi"
launch = ["pi"]
install = "npm install -g --ignore-scripts @earendil-works/pi-coding-agent"

[agent.qwen]
name = "Qwen Code"
launch = ["qwen"]
yolo_args = ["--yolo"]
install = "npm install -g @qwen-code/qwen-code"

[agent.kimi]
name = "Kimi CLI"
launch = ["kimi"]
yolo_args = ["--yolo"]
install = "uv tool install kimi-cli"
"#;

/// 내장 외 확장 프리셋 프로필 목록.
#[must_use]
pub fn preset_profiles() -> &'static BTreeMap<String, Profile> {
    static PARSED: std::sync::LazyLock<BTreeMap<String, Profile>> =
        std::sync::LazyLock::new(|| {
            toml::from_str::<Agents>(PRESETS)
                .map(|a| a.profiles)
                .unwrap_or_default()
        });
    &PARSED
}

/// 에이전트 ID 별칭을 기본 ID로 정규화한다(`"antigravity"` -> `"agy"` 등).
#[must_use]
pub fn canonical_id(id: &str) -> &str {
    let trimmed = id.trim();
    if trimmed.eq_ignore_ascii_case("antigravity") {
        "agy"
    } else if trimmed.eq_ignore_ascii_case("cursor-agent") {
        "cursor"
    } else if trimmed.eq_ignore_ascii_case("github-copilot-cli")
        || trimmed.eq_ignore_ascii_case("gh-copilot")
    {
        "copilot"
    } else if trimmed.eq_ignore_ascii_case("aider-chat") {
        "aider"
    } else if trimmed.eq_ignore_ascii_case("qwen-code") {
        "qwen"
    } else if trimmed.eq_ignore_ascii_case("kimi-cli") {
        "kimi"
    } else {
        trimmed
    }
}

/// Identify a configured executable without changing the command that is launched.
/// Windows wrappers and separators are recognized on every client platform.
#[must_use]
pub fn executable_id(program: &str) -> &str {
    let name = program.trim().rsplit(['/', '\\']).next().unwrap_or(program);
    let name = [".exe", ".cmd", ".bat"]
        .iter()
        .find_map(|ext| {
            let split = name.len().checked_sub(ext.len())?;
            name.get(split..)
                .filter(|suffix| suffix.eq_ignore_ascii_case(ext))?;
            name.get(..split)
        })
        .unwrap_or(name);
    for id in ["claude", "codex", "agy", "opencode"] {
        if name.eq_ignore_ascii_case(id) {
            return id;
        }
    }
    if name.eq_ignore_ascii_case("antigravity-cli") {
        return "agy";
    }
    if name.eq_ignore_ascii_case("open-code") {
        return "opencode";
    }
    if name.eq_ignore_ascii_case("claude-code") || name.eq_ignore_ascii_case("claudecode") {
        return "claude";
    }
    canonical_id(name)
}

/// UI·단축키(`⌥⌘1~4`) 및 설정 목록에서 내장 에이전트의 표준 표시 순서.
#[must_use]
pub fn profile_rank(id: &str) -> u8 {
    match id {
        "claude" => 0,
        "codex" => 1,
        "agy" => 2,
        "cursor" => 3,
        "opencode" => 4,
        _ => 5,
    }
}

/// 사용자 프로필 파일 이름. 상태 디렉터리([`crate::daemon::state_dir`]) 아래에 둔다.
pub const FILE_NAME: &str = "agents.toml";

/// `--version`을 기다리는 최대 시간. 멈춘 CLI 하나가 `krill agent list`를 붙잡지 않게 한다.
const VERSION_TIMEOUT: Duration = Duration::from_secs(5);

/// 에이전트 권한 모드. 전역 토글 하나다.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Permission {
    /// 에이전트 자체 기본 권한(매번 묻기).
    Manual,
    /// 프로필의 `yolo_args`를 붙인다. 켜진 pane에는 배지를 단다.
    #[default]
    Yolo,
}

/// 에이전트 하나.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Profile {
    /// 사람이 읽는 이름.
    pub name: String,
    /// 실행 argv. 첫 원소가 PATH에서 찾는 프로그램이다.
    pub launch: Vec<String>,
    /// YOLO일 때만 붙이는 인자.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub yolo_args: Vec<String>,
    /// 미설치일 때 보여 줄 설치 명령.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub install: String,
    /// Windows에서 `install` 대신 보여 줄 `PowerShell` 설치 명령. 비면 `install`을 쓴다.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub install_windows: String,
    /// 자식 환경에 덮어쓰는 값(OWE-657). 없으면 기존 `agents.toml`과 같다.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub env: BTreeMap<String, String>,
}

impl Profile {
    /// PATH에서 찾는 프로그램.
    pub fn program(&self) -> Option<&str> {
        self.launch.first().map(String::as_str)
    }

    /// 이 플랫폼에서 보여 줄 설치 명령.
    #[must_use]
    pub fn install_command(&self) -> &str {
        if cfg!(windows) && !self.install_windows.is_empty() {
            &self.install_windows
        } else {
            &self.install
        }
    }

    /// 실행 argv. `yolo_args`는 [`Permission::Yolo`]일 때만 붙는다.
    pub fn argv(&self, permission: Permission) -> Vec<String> {
        let yolo = match permission {
            Permission::Manual => &[][..],
            Permission::Yolo => &self.yolo_args,
        };
        self.launch.iter().chain(yolo).cloned().collect()
    }
}

/// `KEY=VALUE` 줄 목록을 환경 맵으로 읽는다. 빈 줄은 건너뛴다.
///
/// # Errors
/// `=`이 없거나 키가 비었거나 키에 공백·NUL이 있는 줄의 번호(1부터)를 돌려준다.
pub fn parse_env_lines(text: &str) -> Result<BTreeMap<String, String>, usize> {
    let mut env = BTreeMap::new();
    for (i, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let bad = || i.saturating_add(1);
        let (key, value) = line.split_once('=').ok_or_else(bad)?;
        let key = key.trim();
        if key.is_empty() || key.contains(|c: char| c.is_whitespace() || c == '\0') {
            return Err(bad());
        }
        env.insert(key.to_owned(), value.trim().to_owned());
    }
    Ok(env)
}

/// [`parse_env_lines`]의 역. 키 순서로 한 줄에 하나.
#[must_use]
pub fn format_env_lines(env: &BTreeMap<String, String>) -> String {
    env.iter()
        .map(|(k, v)| format!("{k}={v}"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// 사용자 `agents.toml` 텍스트에서 `[agent.<id>]`를 `profile`로 바꾼 새 텍스트. 다른 프로필·전역 키·
/// 그 항목의 모르는 키·원문 주석을 보존한다.
///
/// # Errors
/// 기존 텍스트가 TOML이 아니거나 `agent`/`agent.<id>`가 테이블이 아니면 실패한다.
pub fn set_user_profile(user: Option<&str>, id: &str, profile: &Profile) -> Result<String, String> {
    let original = user.unwrap_or_default();
    let mut agents: Agents = toml::from_str(original).map_err(|e| e.to_string())?;
    agents.profiles.insert(id.to_owned(), profile.clone());
    crate::config_document::edit_toml(original, &agents).map_err(|e| e.to_string())
}

/// 전역 권한과 프로필 전부(id 순).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Agents {
    #[serde(default)]
    pub permission: Permission,
    #[serde(default, rename = "agent")]
    pub profiles: BTreeMap<String, Profile>,
}

impl Agents {
    /// 내장 프로필에 사용자 TOML을 겹친다. 같은 id는 사용자 것이 통째로 이긴다.
    pub fn load(user: Option<&str>) -> Result<Self, toml::de::Error> {
        let mut agents: Self = toml::from_str(BUILTIN)?;
        if let Some(text) = user {
            let user: Self = toml::from_str(text)?;
            agents.permission = user.permission;
            agents.profiles.extend(user.profiles);
        }
        agents.profiles.remove("gemini");
        Ok(agents)
    }

    /// `id` 또는 정규화한 기본 ID(`"antigravity"` -> `"agy"`)로 프로필과 실제 키를 찾는다.
    /// `self.profiles`에 없으면 [`preset_profiles`]에서 찾는다(MK-229).
    #[must_use]
    pub fn resolve<'a>(&'a self, id: &str) -> Option<(&'a str, &'a Profile)> {
        if let Some((k, p)) = self.profiles.get_key_value(id) {
            return Some((k.as_str(), p));
        }
        let canon = canonical_id(id);
        if let Some((k, p)) = self.profiles.get_key_value(canon) {
            return Some((k.as_str(), p));
        }
        let presets = preset_profiles();
        if let Some((k, p)) = presets.get_key_value(id) {
            return Some((k.as_str(), p));
        }
        presets.get_key_value(canon).map(|(k, p)| (k.as_str(), p))
    }

    /// 내장·사용자 프로필과 [`preset_profiles`]를 합친 전체 프로필 맵(사용자·내장 우선).
    #[must_use]
    pub fn all_profiles_with_presets(&self) -> BTreeMap<String, Profile> {
        let mut all = preset_profiles().clone();
        for (k, v) in &self.profiles {
            all.insert(k.clone(), v.clone());
        }
        all
    }
}

const LAUNCH_OVERRIDE_PREFIX: &str = "launch\x1f";

/// 에이전트 세션 기동 시 프로필 기본값 위에 얹는 런타임 모델·추론 강도 오버라이드.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LaunchOverrides {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
}

impl LaunchOverrides {
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.model.is_none() && self.effort.is_none()
    }
}

fn validate_model_syntax(raw: &str) -> Result<String, String> {
    let model = raw.trim();
    if model.is_empty() {
        return Err("model must not be empty".to_owned());
    }
    if model.len() > 128 {
        return Err("model exceeds 128 bytes".to_owned());
    }
    if model.starts_with('-') {
        return Err(format!("model must not start with '-': {model}"));
    }
    if !model
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-' | ':' | '/' | '@'))
    {
        return Err(format!(
            "model contains unsupported characters: {model} (allowed: ASCII alphanumeric, '.', '_', '-', ':', '/', '@')"
        ));
    }
    Ok(model.to_owned())
}

fn validate_effort_syntax(raw: &str) -> Result<String, String> {
    let effort = raw.trim().to_ascii_lowercase();
    if effort.is_empty() {
        return Err("effort must not be empty".to_owned());
    }
    if !matches!(
        effort.as_str(),
        "minimal" | "low" | "medium" | "high" | "xhigh" | "max"
    ) {
        return Err(format!(
            "unsupported effort '{raw}'; expected one of: minimal, low, medium, high, xhigh, max"
        ));
    }
    Ok(effort)
}

fn has_model_flag_in_args(args: &[String]) -> bool {
    args.iter().any(|arg| {
        arg == "--model" || arg == "-m" || arg.starts_with("--model=") || arg.starts_with("-m=")
    })
}

fn has_effort_flag_in_args(args: &[String]) -> bool {
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        if arg == "--effort"
            || arg.starts_with("--effort=")
            || arg == "--thinking"
            || arg.starts_with("--thinking=")
            || arg.contains("model_reasoning_effort")
        {
            return true;
        }
        if (arg == "-c" || arg == "--config")
            && iter
                .next()
                .is_some_and(|val| val.contains("model_reasoning_effort"))
        {
            return true;
        }
    }
    false
}

/// 에이전트 ID와 프로필에 대해 `overrides`가 문법·공급자·프로필 인자와 충돌하지 않는지 검증하고
/// 정규화된 [`LaunchOverrides`]를 돌려준다.
pub fn validate_launch_overrides(
    agent_id: &str,
    profile: &Profile,
    overrides: &LaunchOverrides,
) -> Result<LaunchOverrides, String> {
    let canon = canonical_id(agent_id);
    let model = match overrides.model.as_deref() {
        Some(raw) => {
            let valid = validate_model_syntax(raw)?;
            if !matches!(
                canon,
                "claude" | "codex" | "agy" | "opencode" | "cursor" | "pi" | "kimi"
            ) {
                return Err(format!(
                    "--model override is not supported for agent '{agent_id}'"
                ));
            }
            if has_model_flag_in_args(&profile.launch) || has_model_flag_in_args(&profile.yolo_args)
            {
                return Err(format!(
                    "agent profile '{agent_id}' already defines a model flag in launch/yolo_args"
                ));
            }
            Some(valid)
        }
        None => None,
    };

    let effort = match overrides.effort.as_deref() {
        Some(raw) => {
            let valid = validate_effort_syntax(raw)?;
            match canon {
                "codex" => {
                    if !matches!(
                        valid.as_str(),
                        "minimal" | "low" | "medium" | "high" | "xhigh"
                    ) {
                        return Err(format!(
                            "agent '{agent_id}' supports effort: minimal, low, medium, high, xhigh (got '{valid}')"
                        ));
                    }
                }
                "claude" => {
                    if !matches!(valid.as_str(), "low" | "medium" | "high" | "max") {
                        return Err(format!(
                            "agent '{agent_id}' supports effort: low, medium, high, max (got '{valid}')"
                        ));
                    }
                }
                "pi" => {
                    if !matches!(
                        valid.as_str(),
                        "minimal" | "low" | "medium" | "high" | "xhigh"
                    ) {
                        return Err(format!(
                            "agent '{agent_id}' supports effort: minimal, low, medium, high, xhigh (got '{valid}')"
                        ));
                    }
                }
                _ => {
                    return Err(format!(
                        "--effort override is not supported for agent '{agent_id}' (supported: claude, codex, pi)"
                    ));
                }
            }
            if has_effort_flag_in_args(&profile.launch)
                || has_effort_flag_in_args(&profile.yolo_args)
            {
                return Err(format!(
                    "agent profile '{agent_id}' already defines an effort/thinking flag in launch/yolo_args"
                ));
            }
            Some(valid)
        }
        None => None,
    };

    Ok(LaunchOverrides { model, effort })
}

/// 검증된 [`LaunchOverrides`]를 `profile.launch`에 반영한다.
pub fn apply_launch_overrides(
    agent_id: &str,
    profile: &mut Profile,
    overrides: &LaunchOverrides,
) -> Result<(), String> {
    let normalized = validate_launch_overrides(agent_id, profile, overrides)?;
    let canon = canonical_id(agent_id);
    if let Some(model) = normalized.model {
        profile.launch.push("--model".to_owned());
        profile.launch.push(model);
    }
    if let Some(effort) = normalized.effort {
        match canon {
            "codex" => {
                profile.launch.push("-c".to_owned());
                profile
                    .launch
                    .push(format!("model_reasoning_effort=\"{effort}\""));
            }
            "claude" => {
                profile.launch.push("--effort".to_owned());
                profile.launch.push(effort);
            }
            "pi" => {
                profile.launch.push("--thinking".to_owned());
                profile.launch.push(effort);
            }
            _ => {
                return Err(format!(
                    "--effort override is not supported for agent '{agent_id}'"
                ));
            }
        }
    }
    Ok(())
}

/// `Program::Agent { id }` 필드에 에이전트 ID와 [`LaunchOverrides`]를 인코딩한다.
/// 오버라이드가 비어 있으면 기존 프로토콜과 100% 동일하게 `agent_id` 그대로 돌려준다.
pub fn encode_agent_program_id(
    agent_id: &str,
    overrides: &LaunchOverrides,
) -> Result<String, String> {
    let agent_id = agent_id.trim();
    if agent_id.is_empty() || agent_id.contains('\x1f') || agent_id.contains('\0') {
        return Err("invalid agent id".to_owned());
    }
    if overrides.is_empty() {
        return Ok(agent_id.to_owned());
    }
    let model = match overrides.model.as_deref() {
        Some(raw) => validate_model_syntax(raw)?,
        None => String::new(),
    };
    let effort = match overrides.effort.as_deref() {
        Some(raw) => validate_effort_syntax(raw)?,
        None => String::new(),
    };
    Ok(format!(
        "{LAUNCH_OVERRIDE_PREFIX}{agent_id}\x1f{model}\x1f{effort}"
    ))
}

/// [`encode_agent_program_id`]로 인코딩된 `Program::Agent { id }`를 `(agent_id, overrides)`로 복원한다.
pub fn decode_agent_program_id(encoded: &str) -> Result<(&str, LaunchOverrides), String> {
    let Some(rest) = encoded.strip_prefix(LAUNCH_OVERRIDE_PREFIX) else {
        if encoded.is_empty() || encoded.contains('\x1f') || encoded.contains('\0') {
            return Err("invalid agent id".to_owned());
        }
        return Ok((encoded, LaunchOverrides::default()));
    };
    let mut parts = rest.split('\x1f');
    let agent_id = parts
        .next()
        .filter(|s| !s.is_empty() && !s.contains('\0'))
        .ok_or_else(|| "invalid launch override encoding: missing agent id".to_owned())?;
    let model_raw = parts
        .next()
        .ok_or_else(|| "invalid launch override encoding: missing model slot".to_owned())?;
    let effort_raw = parts
        .next()
        .ok_or_else(|| "invalid launch override encoding: missing effort slot".to_owned())?;
    if parts.next().is_some() {
        return Err("invalid launch override encoding: unexpected extra fields".to_owned());
    }
    let model = if model_raw.is_empty() {
        None
    } else {
        Some(validate_model_syntax(model_raw)?)
    };
    let effort = if effort_raw.is_empty() {
        None
    } else {
        Some(validate_effort_syntax(effort_raw)?)
    };
    Ok((agent_id, LaunchOverrides { model, effort }))
}

/// `program`을 `path`(PATH 형식)에서 찾는다. 경로 구분자(`/`, Windows는 `\`도)가 들어 있으면 그 경로만 본다
/// (셸 `command -v`와 같은 규칙).
pub fn which(program: &str, path: &OsStr) -> Option<PathBuf> {
    if program.is_empty() {
        return None;
    }
    if program.contains(std::path::is_separator) {
        return executable(PathBuf::from(program));
    }
    std::env::split_paths(path)
        .filter(|d| !d.as_os_str().is_empty())
        .find_map(|d| executable(d.join(program)))
}

/// 실행 비트가 있는 일반 파일이면 그 경로.
#[cfg(unix)]
fn executable(p: PathBuf) -> Option<PathBuf> {
    use std::os::unix::fs::PermissionsExt as _;
    p.metadata()
        .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        .then_some(p)
}

/// Windows에는 실행 비트가 없어 `PATHEXT`(없으면 `.COM;.EXE;.BAT;.CMD`) 확장자를 붙여 본다(`claude` → `claude.cmd`).
/// 이미 확장자가 있으면 그 이름을 먼저 본다.
#[cfg(windows)]
fn executable(p: PathBuf) -> Option<PathBuf> {
    if p.extension().is_some() && p.is_file() {
        return Some(p);
    }
    let exts = std::env::var("PATHEXT").unwrap_or_else(|_| ".COM;.EXE;.BAT;.CMD".to_owned());
    exts.split(';').filter(|e| !e.is_empty()).find_map(|ext| {
        let mut name = p.clone().into_os_string();
        name.push(ext.to_ascii_lowercase());
        let candidate = PathBuf::from(name);
        candidate.is_file().then_some(candidate)
    })
}

/// `<exe> --version` 출력의 첫 줄. 실패하거나 [`VERSION_TIMEOUT`] 안에 끝나지 않으면 `None`.
pub fn version(exe: &Path) -> Option<String> {
    let mut cmd = crate::os::background_command(exe);
    cmd.arg("--version");
    capture(cmd, VERSION_TIMEOUT)?
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .map(str::to_owned)
}

/// 명령의 표준 출력 전체(stderr 버림). 실패하거나 `timeout` 안에 끝나지 않으면 `None`이고 자식은 죽인다.
pub fn capture(mut cmd: Command, timeout: Duration) -> Option<String> {
    let mut child = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let mut stdout = child.stdout.take()?;
    // 손자 프로세스가 파이프를 쥐고 있으면 읽기가 끝나지 않으므로 읽기를 따로 돌리고 시간으로 끊는다.
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut text = String::new();
        tx.send(stdout.read_to_string(&mut text).map(|_| text)).ok();
    });
    let out = rx.recv_timeout(timeout);
    // 이미 끝났으면 좀비라 kill은 무해하다.
    child.kill().ok();
    child.wait().ok();
    out.ok()?.ok()
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod executable_identity_tests {
    use super::*;
    #[test]
    fn provider_identity_accepts_windows_wrappers_without_rewriting_program() {
        for program in [r"C:\Tools\Claude.EXE", "/tools/claude.cmd", "CLAUDE.bat"] {
            assert_eq!(executable_id(program), "claude");
            assert_eq!(
                crate::usage::ProviderKind::parse(program),
                Some(crate::usage::ProviderKind::Claude)
            );
        }
        assert_eq!(executable_id(r"\\server\cli\Antigravity.cmd"), "agy");
        assert_eq!(executable_id("claude.exe.backup"), "claude.exe.backup");
    }
}
