//! 에이전트 전역 설정과 생성 당시 권한. 설정·메타데이터를 spawn 전에 저장한다.
//!
//! 서명 규칙 파일(MK-091)이 있고 서명이 맞으면 프로필·화면 규칙을 겹친다. 없거나
//! 틀리면 내장 규칙을 유지한다. 전역 권한은 서명 파일로 바꾸지 않는다.
use std::fs::{self, DirBuilder, OpenOptions};
use std::io::{self, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use krill_proto::SessionId;
use krill_proto::agent::rules::{PUBLIC_KEY_FILE_NAME, RULES_FILE_NAME, RulesPayload, ScreenRules};
use krill_proto::agent::{Agents, FILE_NAME, Permission};
use krill_proto::control::{
    AgentLaunch, AgentProfile, AgentSettings, MAX_AGENT_PROFILES, MAX_AGENT_TEXT,
};

use crate::session::lock;

/// 디스크(`agent-launches/<id>.json`)에 남기는 에이전트 실행 기록.
/// `session_id`와 `cwd`는 `#[serde(default)]`라 구 버전 기록도 그대로 읽는다.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct StoredLaunch {
    pub profile: String,
    pub permission: Permission,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub cwd: Vec<u8>,
    /// 구 기록은 roster 관측이 없으므로 자동 종료를 허용하지 않는다.
    #[serde(default = "unknown_roster")]
    pub hibernation_blocked: bool,
    /// 실행에 쓴 계정(OWE-698). 구 기록은 `None`이라 재개가 대화가 든 프로필을 찾는다.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account: Option<LaunchAccount>,
}

/// 실행 당시 적용한 계정 프로필(OWE-698). 재개·휴면 깨우기는 지금 활성 계정이 아니라 이 프로필로 이어 간다.
/// `dir`이 비면 호스트 기본 프로필(환경 변수를 덮어쓰지 않음)이다. `id`는 모르면 빈 값이다(Orca 감지 계정 등).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct LaunchAccount {
    #[serde(default)]
    pub id: String,
    pub env_key: String,
    #[serde(default)]
    pub dir: String,
}

#[derive(Debug)]
pub(crate) struct Control {
    dir: Option<PathBuf>,
    /// 한 데몬의 설정 변경·조회가 서로 덮어쓰지 않게 한다.
    config: Mutex<()>,
    veto: Mutex<std::collections::HashSet<SessionId>>,
}

impl Control {
    pub fn new(dir: Option<&Path>) -> Self {
        Self {
            dir: dir.map(Path::to_owned),
            config: Mutex::new(()),
            veto: Mutex::default(),
        }
    }

    fn user(&self) -> io::Result<Option<String>> {
        let Some(dir) = &self.dir else {
            return Ok(None);
        };
        match fs::read_to_string(dir.join(FILE_NAME)) {
            Ok(s) => Ok(Some(s)),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e),
        }
    }

    pub fn agents(&self) -> io::Result<Agents> {
        let _guard = lock(&self.config);
        self.agents_locked(self.user()?.as_deref())
    }

    /// 서명 규칙이 유효하면 그 화면 규칙, 아니면 내장 규칙. 서명 파일 교체는 다음
    /// 조회부터 반영된다(재시작 없음).
    pub fn screen_rules(&self) -> ScreenRules {
        let _guard = lock(&self.config);
        self.signed()
            .map(|signed| signed.screen)
            .unwrap_or_default()
    }

    /// 서명 규칙(있고 서명이 맞을 때). 없거나 틀리면 `None` → 호출자는 내장을 유지한다.
    fn signed(&self) -> Option<RulesPayload> {
        let dir = self.dir.as_ref()?;
        let key_text = fs::read_to_string(dir.join(PUBLIC_KEY_FILE_NAME)).ok()?;
        let public = krill_proto::agent::rules::decode_public_key(&key_text)?;
        let bytes = fs::read(dir.join(RULES_FILE_NAME)).ok()?;
        match krill_proto::agent::rules::load_signed(&public, &bytes) {
            Ok(signed) => Some(signed),
            Err(e) => {
                tracing::warn!("signed agent rules rejected, keeping builtin: {e}");
                None
            }
        }
    }

    /// 내장 ← 서명 ← 사용자 순으로 겹친다. 사용자 파일이 마지막이라 로컬이 우선이다.
    fn agents_locked(&self, user: Option<&str>) -> io::Result<Agents> {
        let mut agents = Agents::load(None).map_err(io::Error::other)?;
        if let Some(signed) = self.signed() {
            signed.apply(&mut agents);
        }
        if let Some(text) = user {
            let overlay: Agents = toml::from_str(text).map_err(io::Error::other)?;
            agents.permission = overlay.permission;
            agents.profiles.extend(overlay.profiles);
        }
        check_limits(&agents)?;
        Ok(agents)
    }

    pub fn settings(&self, permission: Option<Permission>) -> io::Result<AgentSettings> {
        let _guard = lock(&self.config);
        let user = self.user()?;
        let mut agents = self.agents_locked(user.as_deref())?;
        if let Some(permission) = permission {
            let dir = self.dir.as_ref().ok_or(io::ErrorKind::NotFound)?;
            // 사용자 프로필·모르는 확장 키도 보존한다. TOML 주석·공백은 정규화된다.
            let mut table: toml::Table =
                toml::from_str(user.as_deref().unwrap_or_default()).map_err(io::Error::other)?;
            table.insert(
                "permission".into(),
                toml::Value::String(
                    match permission {
                        Permission::Manual => "manual",
                        Permission::Yolo => "yolo",
                    }
                    .into(),
                ),
            );
            let text = toml::to_string(&table).map_err(io::Error::other)?;
            save(&dir.join(FILE_NAME), text.as_bytes())?;
            agents.permission = permission;
        }
        let mut profiles: Vec<_> = agents
            .profiles
            .into_iter()
            .map(|(id, p)| AgentProfile { id, name: p.name })
            .collect();
        profiles.sort_by(|a, b| {
            krill_proto::agent::profile_rank(&a.id)
                .cmp(&krill_proto::agent::profile_rank(&b.id))
                .then_with(|| a.id.cmp(&b.id))
        });
        Ok(AgentSettings {
            permission: agents.permission,
            profiles,
        })
    }

    fn launch_path(&self, session: SessionId) -> io::Result<PathBuf> {
        Ok(self
            .dir
            .as_ref()
            .ok_or(io::ErrorKind::NotFound)?
            .join("agent-launches")
            .join(format!("{}.json", session.0)))
    }

    pub fn has(&self, session: SessionId) -> bool {
        self.launch_path(session).is_ok_and(|p| p.exists())
    }

    #[cfg_attr(not(test), expect(dead_code, reason = "단위 테스트용 축약 진입점"))]
    pub fn remember(&self, session: SessionId, launch: &AgentLaunch) -> io::Result<()> {
        self.remember_session(session, launch, None, Vec::new(), None)
    }

    pub fn remember_session(
        &self,
        session: SessionId,
        launch: &AgentLaunch,
        session_id: Option<String>,
        cwd: Vec<u8>,
        account: Option<LaunchAccount>,
    ) -> io::Result<()> {
        let _guard = lock(&self.config);
        let stored = StoredLaunch {
            profile: launch.profile.clone(),
            permission: launch.permission,
            session_id,
            cwd,
            hibernation_blocked: false,
            account,
        };
        let bytes = serde_json::to_vec(&stored).map_err(io::Error::other)?;
        save(&self.launch_path(session)?, &bytes)
    }

    pub fn stored(&self, session: SessionId) -> io::Result<Option<StoredLaunch>> {
        let _guard = lock(&self.config);
        self.read_stored(session)
    }

    fn read_stored(&self, session: SessionId) -> io::Result<Option<StoredLaunch>> {
        if self.dir.is_none() {
            return Ok(None);
        }
        match fs::read(self.launch_path(session)?) {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .map(Some)
                .map_err(io::Error::other),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e),
        }
    }

    pub fn launch(&self, session: SessionId) -> io::Result<Option<AgentLaunch>> {
        Ok(self.stored(session)?.map(|s| AgentLaunch {
            profile: s.profile,
            permission: s.permission,
        }))
    }

    /// hook JSON payload에 `"session_id"`·`"conversation_id"`가 있으면 디스크 기록에 적어 둔다(MK-052).
    pub fn record_hook_session(&self, session: SessionId, payload: &[u8]) {
        let payload = serde_json::from_slice::<serde_json::Value>(payload);
        let id = payload
            .as_ref()
            .ok()
            .and_then(|p| {
                p.get("session_id")
                    .or_else(|| p.get("conversation_id"))
                    .or_else(|| p.get("conversationId"))
            })
            .and_then(serde_json::Value::as_str)
            .filter(|s| valid_session_id(s));
        // provider 내부 roster는 종료를 추정하지 않는다. 한 번 사용하면 이 프로세스 수명 동안 veto한다.
        let blocked = payload.as_ref().map_or(true, |p| {
            let event = p
                .get("hook_event_name")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("");
            let tool: String = p
                .get("tool_name")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("")
                .chars()
                .filter(char::is_ascii_alphanumeric)
                .flat_map(char::to_lowercase)
                .collect();
            event.starts_with("Subagent")
                || event.starts_with("Teammate")
                || matches!(
                    tool.as_str(),
                    "task" | "agent" | "spawnagent" | "createteam" | "teamcreate"
                )
        });
        if blocked {
            self.block_hibernation(session);
        }
        let _guard = lock(&self.config);
        let Ok(Some(mut stored)) = self.read_stored(session) else {
            return;
        };
        if id.is_none_or(|id| stored.session_id.as_deref() == Some(id))
            && (!blocked || stored.hibernation_blocked)
        {
            return;
        }
        if let Some(id) = id {
            stored.session_id = Some(id.to_owned());
        }
        stored.hibernation_blocked |= blocked;
        if let Ok(path) = self.launch_path(session)
            && let Ok(bytes) = serde_json::to_vec(&stored)
        {
            save(&path, &bytes).ok();
        }
    }

    pub fn block_hibernation(&self, session: SessionId) {
        lock(&self.veto).insert(session);
    }

    pub fn hibernation_blocked(&self, session: SessionId) -> bool {
        lock(&self.veto).contains(&session)
    }

    pub fn forget(&self, session: SessionId) {
        lock(&self.veto).remove(&session);
        let _guard = lock(&self.config);
        if let Ok(path) = self.launch_path(session) {
            fs::remove_file(path).ok();
        }
    }
}

fn unknown_roster() -> bool {
    true
}

/// 대화 ID를 옵션이나 경로로 해석하지 않게 ASCII 식별자로 제한한다.
pub(crate) fn valid_session_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= MAX_AGENT_TEXT
        && !id.starts_with('-')
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-'))
}

fn check_limits(agents: &Agents) -> io::Result<()> {
    if agents.profiles.len() > MAX_AGENT_PROFILES
        || agents
            .profiles
            .iter()
            .any(|(id, profile)| id.len() > MAX_AGENT_TEXT || profile.name.len() > MAX_AGENT_TEXT)
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "agent profiles exceed limit",
        ));
    }
    Ok(())
}

fn save(path: &Path, bytes: &[u8]) -> io::Result<()> {
    if let Some(dir) = path.parent() {
        DirBuilder::new().recursive(true).mode(0o700).create(dir)?;
    }
    let tmp = path.with_extension("tmp");
    let mut file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&tmp)?;
    file.write_all(bytes)?;
    drop(file);
    fs::rename(tmp, path)
}

#[cfg(test)]
mod tests;
