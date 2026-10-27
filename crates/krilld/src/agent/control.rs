//! 에이전트 전역 설정과 생성 당시 권한. 설정·메타데이터를 spawn 전에 저장한다.
//!
//! 서명 규칙 파일(MK-091)이 있고 서명이 맞으면 프로필·화면 규칙을 겹친다. 없거나
//! 틀리면 내장 규칙을 유지한다. 전역 권한은 서명 파일로 바꾸지 않는다.
use krill_proto::os::fs::{DirBuilderExt, OpenOptionsExt};
use std::fs::{self, DirBuilder, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use krill_proto::SessionId;
use krill_proto::agent::rules::{PUBLIC_KEY_FILE_NAME, RULES_FILE_NAME, RulesPayload, ScreenRules};
use krill_proto::agent::{Agents, FILE_NAME, Permission};
use krill_proto::control::{
    AgentLaunch, AgentProfile, AgentSettings, MAX_AGENT_PROFILES, MAX_AGENT_TEXT,
};

use crate::session::lock;
pub(crate) mod cache;

/// 디스크(`agent-launches/<id>.json`)에 남기는 에이전트 실행 기록.
/// `session_id`와 `cwd`는 `#[serde(default)]`라 구 버전 기록도 그대로 읽는다.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct StoredLaunch {
    pub profile: String,
    pub permission: Permission,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transcript_path: Option<String>,
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
    signed_cache: Mutex<Option<cache::Signed>>,
    veto: Mutex<std::collections::HashSet<SessionId>>,
    /// Only in-flight account replacements are watched. Launch metadata is an expectation,
    /// while this observation comes from a hook attributed to the replacement process.
    account_resumes: Mutex<std::collections::HashMap<SessionId, Option<String>>>,
    /// 한도 초과(`StopFailure` / `agy_rate_limited`)로 중단돼 계정 전환 뒤 작업을 이어갈 세션.
    rate_limited: Mutex<std::collections::HashSet<SessionId>>,
}

impl Control {
    pub fn new(dir: Option<&Path>) -> Self {
        Self {
            dir: dir.map(Path::to_owned),
            config: Mutex::new(()),
            signed_cache: Mutex::new(None),
            veto: Mutex::default(),
            account_resumes: Mutex::default(),
            rate_limited: Mutex::default(),
        }
    }

    pub fn mark_rate_limited(&self, session: SessionId) {
        lock(&self.rate_limited).insert(session);
    }

    pub fn clear_rate_limited(&self, session: SessionId) {
        lock(&self.rate_limited).remove(&session);
    }

    pub fn take_rate_limited(&self, session: SessionId) -> bool {
        lock(&self.rate_limited).remove(&session)
    }

    pub fn watch_account_resume(&self, session: SessionId) {
        lock(&self.account_resumes).insert(session, None);
    }

    pub fn observe_account_resume(&self, session: SessionId, id: &str) {
        if valid_session_id(id)
            && let Some(observed) = lock(&self.account_resumes).get_mut(&session)
        {
            *observed = Some(id.to_owned());
        }
    }

    pub fn account_resume_observed(&self, session: SessionId, id: &str) -> bool {
        lock(&self.account_resumes)
            .get(&session)
            .and_then(|id| id.as_deref())
            == Some(id)
    }

    pub fn finish_account_resume(&self, session: SessionId) {
        lock(&self.account_resumes).remove(&session);
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
            .map(|signed| signed.screen.clone())
            .unwrap_or_default()
    }

    /// 서명 규칙(있고 서명이 맞을 때). 없거나 틀리면 `None` → 호출자는 내장을 유지한다.
    fn signed(&self) -> Option<Arc<RulesPayload>> {
        let dir = self.dir.as_ref()?;
        let key_path = dir.join(PUBLIC_KEY_FILE_NAME);
        let rules_path = dir.join(RULES_FILE_NAME);
        let stamps = (cache::stamp(&key_path), cache::stamp(&rules_path));
        let mut cached = lock(&self.signed_cache);
        if let Some(old) = cached.as_ref()
            && stamps.0.is_some()
            && stamps.1.is_some()
            && old.stamps == stamps
        {
            return old.payload.clone();
        }
        // Fail closed on deletion/read failures. Do not cache an unknown stamp.
        *cached = None;
        let key_text = fs::read_to_string(&key_path).ok()?;
        let public = krill_proto::agent::rules::decode_public_key(&key_text)?;
        let bytes = fs::read(&rules_path).ok()?;
        let payload = match krill_proto::agent::rules::load_signed(&public, &bytes) {
            Ok(signed) => Some(Arc::new(signed)),
            Err(e) => {
                tracing::warn!("signed agent rules rejected, keeping builtin: {e}");
                None
            }
        };
        if stamps.0.is_some()
            && stamps.1.is_some()
            && stamps == (cache::stamp(&key_path), cache::stamp(&rules_path))
        {
            *cached = Some(cache::Signed {
                stamps,
                payload: payload.clone(),
            });
        }
        payload
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

    #[cfg_attr(
        not(all(test, unix)),
        expect(dead_code, reason = "단위 테스트용 축약 진입점")
    )]
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
            transcript_path: None,
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

    /// Merge process-attributed switch metadata without overwriting a concurrent hook.
    pub fn bind_switch_identity(
        &self,
        session: SessionId,
        discovered: &StoredLaunch,
    ) -> io::Result<StoredLaunch> {
        let _guard = lock(&self.config);
        let mut stored = self
            .read_stored(session)?
            .unwrap_or_else(|| discovered.clone());
        if stored.profile != discovered.profile
            || (stored.session_id.is_some() && stored.session_id != discovered.session_id)
        {
            return Err(io::ErrorKind::InvalidData.into());
        }
        stored.session_id.clone_from(&discovered.session_id);
        let should_upgrade_db_path = stored
            .transcript_path
            .as_deref()
            .is_some_and(is_agy_conversation_db_path)
            && discovered
                .transcript_path
                .as_deref()
                .is_some_and(|p| valid_transcript_path(p) && !is_agy_conversation_db_path(p));
        if stored.transcript_path.is_none() || should_upgrade_db_path {
            stored.transcript_path = discovered
                .transcript_path
                .clone()
                .filter(|p| valid_transcript_path(p));
        }
        if stored.account.is_none() {
            stored.account.clone_from(&discovered.account);
        }
        if self.read_stored(session)?.as_ref() != Some(&stored) {
            let bytes = serde_json::to_vec(&stored).map_err(io::Error::other)?;
            save(&self.launch_path(session)?, &bytes)?;
        }
        Ok(stored)
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

    /// 서브에이전트·팀원의 명시적 훅(`agent_id`·`parent_session_id`·`teammate_id`·`is_subagent`·`Subagent*`·`Teammate*`)인지 판정한다.
    pub fn is_explicit_subagent_hook(payload: &[u8]) -> bool {
        serde_json::from_slice::<serde_json::Value>(payload)
            .is_ok_and(|p| is_explicit_subagent_payload(&p))
    }

    /// 서브에이전트·팀원 훅이거나 이미 바인딩된 부모 `session_id`와 다른 보조 프로세스(`claude -p` 등)의
    /// 외래 훅인지 판정한다(#30067, #29429, #28966).
    pub fn is_subagent_or_foreign_hook(&self, session: SessionId, payload: &[u8]) -> bool {
        let Ok(p) = serde_json::from_slice::<serde_json::Value>(payload) else {
            return false;
        };
        if is_explicit_subagent_payload(&p) {
            return true;
        }
        let Some(id) = extract_hook_session_id(&p) else {
            return false;
        };
        if hook_event(&p).is_some_and(|event| is_legitimate_session_id_rotation(event, &p)) {
            return false;
        }
        let _guard = lock(&self.config);
        self.read_stored(session)
            .ok()
            .flatten()
            .and_then(|s| s.session_id)
            .is_some_and(|existing| existing != id)
    }

    /// hook JSON payload에 `"session_id"`·`"conversation_id"`·`"transcript_path"`가 있으면 디스크 기록에 적어 둔다(MK-052, OWE-944).
    /// 서브에이전트·보조 세션의 외래 훅이면 부모 `session_id`·`transcript_path`를 덮어쓰지 않고 `true`를 반환한다(#30067).
    #[cfg_attr(
        not(all(test, unix)),
        expect(dead_code, reason = "단위 테스트용 축약 진입점")
    )]
    pub fn record_hook_session(&self, session: SessionId, payload: &[u8]) -> bool {
        self.record_hook_session_with_context(session, payload, None, None)
    }

    /// 로그인 셸(`Program::Login`) 안에서 사용자가 직접 실행한 에이전트(`claude`·`codex`·`agy`·`opencode`)가
    /// 첫 훅 이벤트를 보낼 때도 `StoredLaunch`가 없으면 자동 바인딩한다(OWE-944 / MK-241).
    pub fn record_hook_session_with_context(
        &self,
        session: SessionId,
        payload: &[u8],
        agent_hint: Option<&str>,
        cwd_hint: Option<&[u8]>,
    ) -> bool {
        let payload = serde_json::from_slice::<serde_json::Value>(payload);
        let id = payload.as_ref().ok().and_then(extract_hook_session_id);
        let transcript_path = payload.as_ref().ok().and_then(extract_hook_transcript_path);
        let explicit_subagent = payload.as_ref().is_ok_and(is_explicit_subagent_payload);
        // provider 내부 roster는 종료를 추정하지 않는다. 한 번 사용하면 이 프로세스 수명 동안 veto한다.
        let blocked = payload.as_ref().map_or(true, |p| {
            let tool: String = p
                .get("tool_name")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("")
                .chars()
                .filter(char::is_ascii_alphanumeric)
                .flat_map(char::to_lowercase)
                .collect();
            explicit_subagent
                || matches!(
                    tool.as_str(),
                    "task" | "agent" | "spawnagent" | "createteam" | "teamcreate"
                )
        });
        if blocked {
            self.block_hibernation(session);
        }
        let _guard = lock(&self.config);
        let mut stored = match self.read_stored(session) {
            Ok(Some(stored)) => stored,
            Ok(None) => {
                let profile = agent_hint
                    .map(str::trim)
                    .filter(|s| valid_session_id(s))
                    .map(|s| {
                        let c = krill_proto::agent::canonical_id(s);
                        if c.eq_ignore_ascii_case("claude-code") {
                            "claude"
                        } else if c.eq_ignore_ascii_case("codex-cli") {
                            "codex"
                        } else {
                            c
                        }
                    })
                    .or_else(|| infer_profile_from_transcript(transcript_path));
                if payload.is_err()
                    || explicit_subagent
                    || (id.is_none() && transcript_path.is_none() && profile.is_none())
                {
                    return explicit_subagent;
                }
                let payload_cwd = payload
                    .as_ref()
                    .ok()
                    .and_then(|p| p.get("cwd").and_then(serde_json::Value::as_str))
                    .filter(|s| !s.is_empty() && s.len() <= 4096)
                    .map(|s| s.as_bytes().to_vec());
                let cwd = payload_cwd
                    .or_else(|| cwd_hint.filter(|c| !c.is_empty()).map(<[u8]>::to_vec))
                    .unwrap_or_default();
                // 로그인 셸에서 수동 실행된 에이전트는 셸 자체를 종료하면 안 되므로 휴면 종료를 막는다.
                self.block_hibernation(session);
                let bound = StoredLaunch {
                    profile: profile.unwrap_or("claude").to_owned(),
                    permission: Permission::Manual,
                    session_id: id.map(str::to_owned),
                    transcript_path: transcript_path.map(str::to_owned),
                    cwd,
                    hibernation_blocked: true,
                    account: None,
                };
                if let Ok(path) = self.launch_path(session)
                    && let Ok(bytes) = serde_json::to_vec(&bound)
                {
                    save(&path, &bytes).ok();
                }
                return false;
            }
            Err(_) => return explicit_subagent,
        };
        let foreign_session_id = match (stored.session_id.as_deref(), id, payload.as_ref().ok()) {
            (Some(existing), Some(incoming), Some(p)) if existing != incoming => {
                match hook_event(p) {
                    Some(event) => !is_legitimate_session_id_rotation(event, p),
                    None => true,
                }
            }
            _ => false,
        };
        let is_subagent_or_foreign = explicit_subagent || foreign_session_id;
        if !is_subagent_or_foreign
            && let Some(id) = id
            && payload
                .as_ref()
                .ok()
                .and_then(hook_event)
                .is_some_and(|event| {
                    matches!(
                        event,
                        "SessionStart"
                            | "sessionStart"
                            | "PreInvocation"
                            | "UserPromptSubmit"
                            | "PreToolUse"
                    )
                })
        {
            self.observe_account_resume(session, id);
        }

        if foreign_session_id {
            self.block_hibernation(session);
        }
        let next_blocked = blocked || foreign_session_id;
        let had_session_id = stored.session_id.is_some();
        let should_update_id = !is_subagent_or_foreign
            && id.is_some_and(|incoming| stored.session_id.as_deref() != Some(incoming));
        let should_update_transcript = !is_subagent_or_foreign
            && transcript_path
                .is_some_and(|incoming| stored.transcript_path.as_deref() != Some(incoming));
        let should_clear_stale_transcript = should_update_id
            && had_session_id
            && transcript_path.is_none()
            && stored.transcript_path.is_some();
        if !should_update_id
            && !should_update_transcript
            && !should_clear_stale_transcript
            && (!next_blocked || stored.hibernation_blocked)
        {
            return is_subagent_or_foreign;
        }
        if should_update_id && let Some(id) = id {
            stored.session_id = Some(id.to_owned());
        }
        if should_update_transcript && let Some(tp) = transcript_path {
            stored.transcript_path = Some(tp.to_owned());
        } else if should_clear_stale_transcript {
            stored.transcript_path = None;
        }
        stored.hibernation_blocked |= next_blocked;
        if let Ok(path) = self.launch_path(session)
            && let Ok(bytes) = serde_json::to_vec(&stored)
        {
            save(&path, &bytes).ok();
        }
        is_subagent_or_foreign
    }

    /// 실행 중인 에이전트 프로세스(`presence/<id>.lock` 등)에서 발견한 대화 ID·트랜스크립트 경로를
    /// 아직 `session_id` 또는 `transcript_path`가 비어 있는 기존 `StoredLaunch`에 기록한다.
    pub fn bind_discovered_session(
        &self,
        session: SessionId,
        session_id: &str,
        transcript_path: Option<&Path>,
    ) {
        if !valid_session_id(session_id) {
            return;
        }
        let _guard = lock(&self.config);
        let Ok(Some(mut stored)) = self.read_stored(session) else {
            return;
        };
        if stored
            .session_id
            .as_deref()
            .is_some_and(|existing| existing != session_id)
        {
            return;
        }
        let mut changed = false;
        if stored.session_id.is_none() {
            stored.session_id = Some(session_id.to_owned());
            changed = true;
        }
        if let Some(tp) = transcript_path
            .and_then(Path::to_str)
            .map(str::trim)
            .filter(|p| valid_transcript_path(p))
        {
            let should_upgrade_db_path = stored
                .transcript_path
                .as_deref()
                .is_some_and(is_agy_conversation_db_path)
                && !is_agy_conversation_db_path(tp);
            if stored.transcript_path.is_none() || should_upgrade_db_path {
                stored.transcript_path = Some(tp.to_owned());
                changed = true;
            }
        }
        if !changed {
            return;
        }
        if let Ok(path) = self.launch_path(session)
            && let Ok(bytes) = serde_json::to_vec(&stored)
        {
            save(&path, &bytes).ok();
        }
    }

    /// 세션 실행 기록(`agent-launches/<id>.json`)이 생성된 시각(epoch 초).
    pub fn launched_at(&self, session: SessionId) -> Option<u64> {
        let path = self.launch_path(session).ok()?;
        let meta = fs::metadata(path).ok()?;
        meta.created()
            .or_else(|_| meta.modified())
            .ok()?
            .duration_since(std::time::UNIX_EPOCH)
            .ok()
            .map(|d| d.as_secs())
    }

    /// 다른 세션(`exclude` 제외)이 이미 바인딩한 `session_id`와 `transcript_path` 집합을 반환한다.
    pub fn claimed_transcripts(
        &self,
        exclude: SessionId,
    ) -> (
        std::collections::BTreeSet<String>,
        std::collections::BTreeSet<PathBuf>,
    ) {
        let mut sids = std::collections::BTreeSet::new();
        let mut paths = std::collections::BTreeSet::new();
        let Some(dir) = self.dir.as_ref().map(|d| d.join("agent-launches")) else {
            return (sids, paths);
        };
        let _guard = lock(&self.config);
        let Ok(entries) = fs::read_dir(dir) else {
            return (sids, paths);
        };
        let skip_name = format!("{}.json", exclude.0);
        for entry in entries.take(512).flatten() {
            if entry.file_name().to_str() == Some(skip_name.as_str()) {
                continue;
            }
            let Ok(bytes) = fs::read(entry.path()) else {
                continue;
            };
            let Ok(stored) = serde_json::from_slice::<StoredLaunch>(&bytes) else {
                continue;
            };
            if let Some(sid) = stored.session_id.filter(|s| !s.trim().is_empty()) {
                sids.insert(sid);
            }
            if let Some(tp) = stored.transcript_path.filter(|s| !s.trim().is_empty()) {
                paths.insert(PathBuf::from(tp));
            }
        }
        (sids, paths)
    }

    pub fn block_hibernation(&self, session: SessionId) {
        lock(&self.veto).insert(session);
    }

    pub fn hibernation_blocked(&self, session: SessionId) -> bool {
        lock(&self.veto).contains(&session)
    }

    pub fn forget(&self, session: SessionId) {
        self.finish_account_resume(session);
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

fn extract_hook_session_id(p: &serde_json::Value) -> Option<&str> {
    p.get("session_id")
        .or_else(|| p.get("conversation_id"))
        .or_else(|| p.get("conversationId"))
        .and_then(serde_json::Value::as_str)
        .filter(|id| valid_session_id(id))
}

pub(crate) fn is_agy_conversation_db_path(path: &str) -> bool {
    let p = Path::new(path);
    p.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|ext| ext.eq_ignore_ascii_case("db"))
        && p.parent()
            .and_then(Path::file_name)
            .and_then(|n| n.to_str())
            == Some("conversations")
        && p.file_stem()
            .and_then(|s| s.to_str())
            .is_some_and(valid_session_id)
}

pub(crate) fn valid_transcript_path(path: &str) -> bool {
    if path.is_empty() || path.len() > 4096 || path.contains('\0') {
        return false;
    }
    let p = Path::new(path);
    p.is_absolute()
        && !p
            .components()
            .any(|c| matches!(c, std::path::Component::ParentDir))
        && (p.extension().and_then(|e| e.to_str()).is_some_and(|ext| {
            ext.eq_ignore_ascii_case("jsonl") || ext.eq_ignore_ascii_case("json")
        }) || is_agy_conversation_db_path(path))
}

fn extract_hook_transcript_path(p: &serde_json::Value) -> Option<&str> {
    p.get("transcript_path")
        .or_else(|| p.get("transcriptPath"))
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|path| valid_transcript_path(path))
}

fn infer_profile_from_transcript(transcript_path: Option<&str>) -> Option<&'static str> {
    let path = transcript_path?;
    if path.contains(".codex") || path.contains("/codex/") {
        Some("codex")
    } else if path.contains(".gemini") || path.contains("antigravity") {
        Some("agy")
    } else if path.contains("opencode") {
        Some("opencode")
    } else if path.contains(".claude") || path.contains("/claude/") {
        Some("claude")
    } else {
        None
    }
}

fn hook_event(p: &serde_json::Value) -> Option<&str> {
    p.get("hook_event_name")
        .or_else(|| p.get("hookEventName"))
        .or_else(|| p.get("event"))
        .and_then(serde_json::Value::as_str)
}

fn is_explicit_subagent_payload(p: &serde_json::Value) -> bool {
    let event = hook_event(p).unwrap_or("");
    if event.starts_with("Subagent") || event.starts_with("Teammate") {
        return true;
    }
    if p.get("is_subagent")
        .or_else(|| p.get("isSubagent"))
        .and_then(serde_json::Value::as_bool)
        == Some(true)
    {
        return true;
    }
    [
        "agent_id",
        "agentId",
        "subagent_id",
        "subagentId",
        "parent_session_id",
        "parentSessionId",
        "teammate_id",
        "teammateId",
    ]
    .iter()
    .any(|key| {
        p.get(*key)
            .and_then(serde_json::Value::as_str)
            .is_some_and(|s| !s.is_empty())
    })
}

fn is_legitimate_session_id_rotation(event: &str, p: &serde_json::Value) -> bool {
    matches!(event, "SessionStart" | "sessionStart")
        && matches!(
            p.get("source").and_then(serde_json::Value::as_str),
            Some("clear" | "resume" | "compact")
        )
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
#[cfg(unix)]
mod tests;
