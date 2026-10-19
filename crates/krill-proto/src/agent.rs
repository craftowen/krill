//! 에이전트 프로필(MK-037): 내장 5종 + 사용자 `agents.toml`, PATH 감지·버전.
//!
//! 내장 프로필도 사용자 파일과 같은 TOML([`BUILTIN`])이라 사용자가 같은 id로 덮어쓰거나 새 id를 더한다.
//! 권한은 에이전트 자체 기본(Manual)이고, `yolo_args`는 전역 `permission = "yolo"`일 때만 붙는다
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
/// cursor-agent `--force`는 미설치라 미확인. opencode는 권한 우회 플래그가 없어 비워 둔다.
pub const BUILTIN: &str = r#"
[agent.claude]
name = "Claude Code"
launch = ["claude"]
yolo_args = ["--dangerously-skip-permissions"]
install = "curl -fsSL https://claude.ai/install.sh | bash"

[agent.codex]
name = "Codex"
launch = ["codex"]
yolo_args = ["--dangerously-bypass-approvals-and-sandbox"]
install = "npm install -g @openai/codex"

[agent.opencode]
name = "OpenCode"
launch = ["opencode"]
install = "curl -fsSL https://opencode.ai/install | bash"

[agent.cursor]
name = "Cursor CLI"
launch = ["cursor-agent"]
yolo_args = ["--force"]
install = "curl https://cursor.com/install -fsS | bash"

[agent.agy]
name = "Antigravity"
launch = ["agy"]
yolo_args = ["--dangerously-skip-permissions"]
install = "agy install"
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
install = "npm install -g @mariozechner/pi-coding-agent"

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
    #[default]
    Manual,
    /// 프로필의 `yolo_args`를 붙인다. 켜진 pane에는 배지를 단다.
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
    /// 자식 환경에 덮어쓰는 값(OWE-657). 없으면 기존 `agents.toml`과 같다.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub env: BTreeMap<String, String>,
}

impl Profile {
    /// PATH에서 찾는 프로그램.
    pub fn program(&self) -> Option<&str> {
        self.launch.first().map(String::as_str)
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
    let mut cmd = Command::new(exe);
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
