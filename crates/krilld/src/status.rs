//! 에이전트 상태 저장소(MK-039, ARCHITECTURE §6). hook 이벤트([`krill_proto::hook`])와 세션 종료를 세션마다 6상태
//! ([`AgentState`]) + 출처 + 신뢰도로 정규화해 들고 있다. 질의는 [`krill_proto::control::Request::States`].
//!
//! - **정규화**: Claude·Codex 계열 hook의 `hook_event_name`([`normalize`]). 모르는 이벤트는 상태를 바꾸지 않는다.
//!   다른 에이전트의 이벤트 이름은 hook 설치(MK-048)와 같이 붙인다.
//! - **중복 제거**: 같은 이벤트가 소켓과 spool 양쪽으로 올 수 있다(ACK가 늦은 경우). 세션마다 마지막으로 반영한
//!   최신 시각의 [`EventId`]들과 근거 시각을 두고, 같은 이벤트이거나 그보다 오래된 이벤트는 버린다(늦게 수거한 spool 이벤트가 새
//!   상태를 되돌리지 않는다).
//! - **지속**: 바뀔 때마다 `<상태 디렉터리>/`[`FILE_NAME`]에 통째로 쓴다(임시 파일 + rename, fsync 없음). krilld가
//!   재기동해도 상태와 중복 제거 기준이 이어진다. 앱은 상태를 갖지 않으므로 앱 재시작은 영향이 없다.
//! - **spool 수거**([`take_spool`]): krilld가 없을 때 `krill hook`이 쌓은 이벤트. 기동 때와 [`SPOOL_POLL`]마다.
//!
//! # 파일 형식
//!
//! `[u8 버전 = 1][postcard Vec<(SessionId, Record)>]`. 읽지 못하면(다른 버전·손상) 빈 저장소로 시작한다(상태가
//! unknown이 될 뿐 세션에는 영향 없음). 필드는 끝에만 붙이고 버전을 올린다.

use krill_proto::os::fs::OpenOptionsExt as _;
use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::io::{self, Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use krill_proto::SessionId;
use krill_proto::control::{AgentState, AgentStatus, Confidence, Source};
use krill_proto::hook::{self, EventId, HookEvent};
use krill_proto::usage::{AutoSwitchEvent, ProviderKind};
use serde::{Deserialize, Serialize};

use crate::ipc::with_suffix;
use crate::session::lock;
use crate::usage::accounts;

pub mod detect;
mod monitor;
mod subagents;
pub(crate) use monitor::monitor;

/// 상태 디렉터리 안 파일 이름.
pub const FILE_NAME: &str = "agent-state";
const VERSION: u8 = 1;
/// spool을 다시 확인하는 간격. 소켓 전달이 늦어(ACK 시간 초과) spool로 간 이벤트가 이만큼 안에 반영된다.
pub const SPOOL_POLL: Duration = Duration::from_secs(1);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct Record {
    status: AgentStatus,
    /// 최신 시각에 반영한 hook 이벤트들. 같은 시각의 서로 다른 PID도 각각 한 번만 반영한다.
    last: Vec<EventId>,
    /// 마지막으로 반영한 근거의 시각(UNIX 나노초). 이보다 오래된 hook 이벤트는 버린다.
    last_ns: u64,
}

impl Record {
    fn set(&mut self, state: AgentState, source: Source, ns: u64) {
        if self.status.state != state
            || self.status.source.is_none()
            || (source == Source::Hook && state == AgentState::Done)
        {
            self.status.state = state;
            self.status.since_unix_ns = ns;
        }
        self.status.source = Some(source);
        // hook(L1)과 프로세스 종료(L2)는 둘 다 확정 근거다(T4 §3.1.4). 제목·화면(MK-049)은 낮춰 넣는다.
        self.status.confidence = Confidence::High;
        self.last_ns = self.last_ns.max(ns);
    }
}

/// krilld 전체에 하나.
#[derive(Debug)]
pub struct Statuses {
    path: Option<PathBuf>,
    commit: Mutex<()>,
    map: Mutex<BTreeMap<SessionId, Record>>,
    /// 서브에이전트 목록. 저장하지 않는다(krilld 재기동이면 빈 목록).
    subagents: Mutex<BTreeMap<SessionId, subagents::Roster>>,
}

impl Statuses {
    /// `path`의 저장본을 읽는다. 없거나 읽지 못하면 빈 저장소. `None`이면 메모리에만 둔다.
    pub fn open(path: Option<PathBuf>) -> Self {
        let map = path
            .as_deref()
            .and_then(|p| fs::read(p).ok())
            .and_then(|b| decode(&b))
            .unwrap_or_default();
        Self {
            path,
            commit: Mutex::new(()),
            map: Mutex::new(map),
            subagents: Mutex::default(),
        }
    }

    /// 세션의 지금 상태. 근거가 없으면 [`AgentStatus::UNKNOWN`].
    pub fn get(&self, session: SessionId) -> AgentStatus {
        let mut status = lock(&self.map)
            .get(&session)
            .map_or(AgentStatus::UNKNOWN, |r| r.status.clone());
        if let Some(r) = lock(&self.subagents).get(&session) {
            status.subagents = r.list();
        }
        status
    }

    /// 서브에이전트 목록에 hook 하나를 반영한다. `from_parent`는 부모 세션 자신의 이벤트인지다. 바뀌었으면 참.
    pub fn subagent_hook(&self, session: SessionId, e: &HookEvent, from_parent: bool) -> bool {
        let mut map = lock(&self.subagents);
        let r = map.entry(session).or_default();
        let changed = r.observe(&e.payload, e.id.unix_ns, from_parent);
        if r.is_empty() {
            map.remove(&session);
        }
        changed
    }

    /// hook 이벤트들을 저장한 뒤 반영한다. 저장 실패면 메모리·중복 제거 기준도 그대로 두어 재시도할 수 있다.
    pub fn hook(&self, events: &[(SessionId, &HookEvent)]) -> io::Result<usize> {
        let _commit = lock(&self.commit);
        let mut next = lock(&self.map).clone();
        let mut n = 0;
        for &(session, e) in events {
            let Some(state) = normalize(&e.payload) else {
                continue;
            };
            let r = next.entry(session).or_insert_with(|| Record {
                status: AgentStatus::UNKNOWN,
                last: Vec::new(),
                last_ns: 0,
            });
            if r.last.contains(&e.id) || e.id.unix_ns < r.last_ns {
                continue;
            }
            if e.id.unix_ns > r.last_ns {
                r.last.clear();
            }
            r.set(state, Source::Hook, e.id.unix_ns);
            r.status.agent.clone_from(&e.agent);
            r.last.push(e.id);
            n += 1;
        }
        if n > 0 {
            self.save(&next)?;
            *lock(&self.map) = next;
        }
        Ok(n)
    }

    /// 세션 루트 프로세스가 끝났다(holder `Exited`). 그 전에 시작한 hook 이벤트는 더는 상태를 바꾸지 않는다.
    pub fn exited(&self, session: SessionId) {
        lock(&self.subagents).remove(&session);
        let _commit = lock(&self.commit);
        let mut map = lock(&self.map).clone();
        let r = map.entry(session).or_insert_with(|| Record {
            status: AgentStatus::UNKNOWN,
            last: Vec::new(),
            last_ns: 0,
        });
        r.set(AgentState::Exited, Source::Process, now_ns());
        self.save(&map).ok();
        *lock(&self.map) = map;
    }

    /// 제목·화면의 보조 근거. hook이 이기며, 대화상자 소실을 확인한 waiting 해제만 예외다.
    /// 보조 근거 시각으로 hook 중복 제거 시각을 밀지 않는다(늦게 전달된 새 hook도 반영한다).
    pub fn inferred(&self, session: SessionId, evidence: detect::Evidence, ns: u64) {
        let _commit = lock(&self.commit);
        let mut map = lock(&self.map).clone();
        let r = map.entry(session).or_insert_with(|| Record {
            status: AgentStatus::UNKNOWN,
            last: Vec::new(),
            last_ns: 0,
        });
        if r.status.source == Some(Source::Process)
            || (r.status.source == Some(Source::Hook)
                && !(r.status.state == AgentState::Waiting
                    && evidence.dismissed_since == Some(r.status.since_unix_ns)))
        {
            return;
        }
        if r.status.state == evidence.state
            && r.status.source == Some(evidence.source)
            && r.status.confidence == evidence.confidence
        {
            return;
        }
        let hook_ns = r.last_ns;
        r.set(evidence.state, evidence.source, ns);
        r.last_ns = hook_ns;
        r.status.confidence = evidence.confidence;
        self.save(&map).ok();
        *lock(&self.map) = map;
    }

    /// 새 전경 에이전트가 확인되면 이전 에이전트의 종료 표식을 지운다. 셸 작업에는 호출하지 않는다.
    pub(crate) fn started(&self, session: SessionId) {
        let _commit = lock(&self.commit);
        let mut map = lock(&self.map).clone();
        if let Some(r) = map.get_mut(&session)
            && r.status.source == Some(Source::Process)
            && r.status.state == AgentState::Exited
        {
            r.status.state = AgentState::Unknown;
            r.status.source = None;
            r.status.confidence = Confidence::Low;
            r.status.since_unix_ns = now_ns();
            self.save(&map).ok();
            *lock(&self.map) = map;
        }
    }

    /// 지운 세션의 상태를 버린다.
    pub fn remove(&self, session: SessionId) {
        lock(&self.subagents).remove(&session);
        let _commit = lock(&self.commit);
        let mut map = lock(&self.map).clone();
        if map.remove(&session).is_some() {
            self.save(&map).ok();
            *lock(&self.map) = map;
        }
    }

    /// `live`에 없는 세션의 상태를 버린다(krilld 기동 때: 그사이 사라진 세션).
    pub fn retain(&self, live: &[SessionId]) {
        let _commit = lock(&self.commit);
        let mut map = lock(&self.map).clone();
        let before = map.len();
        map.retain(|id, _| live.contains(id));
        if map.len() != before {
            self.save(&map).ok();
            *lock(&self.map) = map;
        }
    }

    fn save(&self, map: &BTreeMap<SessionId, Record>) -> io::Result<()> {
        if let Some(p) = &self.path {
            write_atomic(p, &encode(map)?)?;
        }
        Ok(())
    }
}

fn encode(map: &BTreeMap<SessionId, Record>) -> io::Result<Vec<u8>> {
    let rows: Vec<(SessionId, &Record)> = map.iter().map(|(&id, r)| (id, r)).collect();
    let mut out = vec![VERSION];
    out.extend(postcard::to_stdvec(&rows).map_err(io::Error::other)?);
    Ok(out)
}

fn decode(b: &[u8]) -> Option<BTreeMap<SessionId, Record>> {
    let (&VERSION, rest) = b.split_first()? else {
        return None;
    };
    let rows: Vec<(SessionId, Record)> = postcard::from_bytes(rest).ok()?;
    Some(rows.into_iter().collect())
}

fn write_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let tmp = with_suffix(path, ".tmp");
    OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&tmp)?
        .write_all(bytes)?;
    fs::rename(&tmp, path)
}

pub(crate) fn now_ns() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_nanos()).unwrap_or(u64::MAX))
}

/// hook payload(JSON)에서 읽는 필드. 나머지는 무시한다.
#[derive(Deserialize)]
struct Fields {
    #[serde(alias = "hookEventName", alias = "event")]
    hook_event_name: Option<String>,
    #[serde(alias = "toolName")]
    tool_name: Option<String>,
    #[serde(alias = "notificationType")]
    notification_type: Option<String>,
    #[serde(alias = "agentId", alias = "subagent_id", alias = "subagentId")]
    agent_id: Option<String>,
    #[serde(alias = "parentSessionId")]
    parent_session_id: Option<String>,
    #[serde(alias = "teammateId")]
    teammate_id: Option<String>,
    #[serde(default, alias = "isSubagent")]
    is_subagent: bool,
}

/// Claude·Codex·Cursor·OpenCode·Antigravity hook payload 하나를 상태로. 상태를 바꾸지 않는 이벤트(서브에이전트·압축 등)와
/// JSON이 아닌 payload는 `None`. 이벤트 이름이 에이전트끼리 겹쳐도 뜻이 같아 에이전트별로 나누지 않는다.
///
/// | 이벤트 | 상태 |
/// |---|---|
/// | `SessionStart`, Cursor `sessionStart` | done(프롬프트에서 대기, 턴 완료 여부는 wait 기준점으로 구분) |
/// | `Stop`, `StopFailure`, `Interrupt`, Antigravity `PostInvocation`, Cursor `stop` | done(프롬프트에서 대기) |
/// | `UserPromptSubmit`, `PreToolUse`, `PostToolUse`, Antigravity `PreInvocation`, Cursor `beforeSubmitPrompt`·`afterShellExecution`·`afterMCPExecution` | working |
/// | `PermissionRequest`, 질문 도구(`AskUserQuestion`·`request_user_input`·`ask_question`·`ask_permission`)의 `PreToolUse`/`BeforeTool` | waiting |
/// | `Notification`의 `permission_prompt`·`elicitation_dialog` | waiting(`idle_prompt`는 Stop 약 60초 뒤라 무시) |
/// | `SessionEnd`, Cursor `sessionEnd` | exited |
pub fn normalize(payload: &[u8]) -> Option<AgentState> {
    let f: Fields = serde_json::from_slice(payload).ok()?;
    if f.is_subagent
        || f.agent_id.as_deref().is_some_and(|s| !s.is_empty())
        || f.parent_session_id
            .as_deref()
            .is_some_and(|s| !s.is_empty())
        || f.teammate_id.as_deref().is_some_and(|s| !s.is_empty())
    {
        return None;
    }
    let asks = f.tool_name.as_deref().is_some_and(asks_user);
    Some(match f.hook_event_name.as_deref()? {
        "SessionStart" | "sessionStart" | "Stop" | "StopFailure" | "Interrupt" | "AfterAgent"
        | "PostInvocation" | "stop" => AgentState::Done,
        "PreToolUse" | "BeforeTool" if asks => AgentState::Waiting,
        "UserPromptSubmit"
        | "PreToolUse"
        | "PostToolUse"
        | "BeforeAgent"
        | "BeforeTool"
        | "AfterTool"
        | "PreInvocation"
        | "beforeSubmitPrompt"
        | "afterShellExecution"
        | "afterMCPExecution" => AgentState::Working,
        "PermissionRequest" => AgentState::Waiting,
        "Notification"
            if matches!(
                f.notification_type.as_deref(),
                Some("permission_prompt" | "elicitation_dialog" | "ToolPermission")
            ) =>
        {
            AgentState::Waiting
        }
        "SessionEnd" | "sessionEnd" => AgentState::Exited,
        _ => return None,
    })
}

/// 사람에게 묻는 도구(Claude `AskUserQuestion`, Codex `request_user_input`, Antigravity `ask_question`·`ask_permission`). 대소문자·구분자 무시.
fn asks_user(tool: &str) -> bool {
    let n: String = tool
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .map(|c| c.to_ascii_lowercase())
        .collect();
    matches!(
        n.as_str(),
        "askuserquestion" | "requestuserinput" | "askquestion" | "askpermission"
    )
}

/// Claude `StopFailure` 한도 도달 감지(OWE-710). 정규화는 [`krill_proto::agent::claude`]에 둔다.
/// 상태 판정([`normalize`]: 한도여도 `Done`)과 분리돼, 자동전환 트리거에만 쓴다.
pub fn stop_failure_rate_limited(payload: &[u8]) -> bool {
    krill_proto::agent::claude::is_rate_limit(payload)
}

/// agy 한도 도달 감지(OWE-901). `Stop`·`PostInvocation`의 `error: "rate_limit"` 완전 일치만 본다.
/// Claude `StopFailure`와 같은 상수를 공유해 오탐을 막는다. 그 밖의 문구(쿼터·429 등)는 다루지 않는다.
pub fn agy_rate_limited(payload: &[u8]) -> bool {
    #[derive(Deserialize)]
    struct AgyFields {
        #[serde(alias = "hookEventName", alias = "event")]
        hook_event_name: Option<String>,
        error: Option<String>,
    }
    let Ok(fields): Result<AgyFields, _> = serde_json::from_slice(payload) else {
        return false;
    };
    matches!(
        fields.hook_event_name.as_deref(),
        Some("Stop" | "PostInvocation")
    ) && fields.error.as_deref() == Some(krill_proto::agent::claude::STOP_FAILURE_RATE_LIMIT)
}

/// Claude `StopFailure` 한도 도달이면 자동전환 평가를 즉시 트리거한다(OWE-710).
/// 평가는 `accounts::refresh_accounts_on_rate_limit`으로 기존 정책(활성 계정·임계치·히스테리시스·
/// 수동 선택·후보 제외·`switch_on_rate_limit`)을 그대로 따른다. 전환이 저장되면
/// 계정 작업 스레드가 기존 대화를 선택된 계정 프로필로 재개한다.
/// Claude·Antigravity가 아닌 에이전트·한도 외 오류·비JSON payload면 평가하지 않고 `None`을 돌려준다.
/// agy는 같은 `rate_limit` 봉투(`Stop`·`PostInvocation`의 `error: "rate_limit"`)로 오면 전환한다(OWE-901).
pub fn auto_switch_on_rate_limit(
    home: &Path,
    state_dir: Option<&Path>,
    agent: &str,
    payload: &[u8],
) -> Option<Vec<AutoSwitchEvent>> {
    let limited = match ProviderKind::parse(agent) {
        Some(ProviderKind::Claude) => stop_failure_rate_limited(payload),
        Some(ProviderKind::Antigravity) => agy_rate_limited(payload),
        _ => return None,
    };
    if !limited {
        return None;
    }
    Some(accounts::refresh_accounts_on_rate_limit(home, state_dir).switched)
}

/// spool을 수거한다: flock 아래에서 다 읽고, 이벤트를 시각 순으로 `apply`에 넘긴 뒤 비운다. 반영(저장)한 다음에
/// 비우므로 그 사이 krilld가 죽으면 다음 기동이 다시 읽고 중복 제거가 거른다. 파일이 없거나 비었으면 아무것도 안
/// 한다.
///
/// 락은 `apply` 동안 쥔다(이벤트 몇 개의 세션 판정 + 저장 한 번). `krill hook`은 락을 수 ms 기다린다
/// ([`hook::append_spool`]).
///
/// # Errors
/// 열기·락·읽기·반영·비우기 실패. 저장 실패면 spool을 보존해 다음 수거가 다시 읽는다.
pub fn take_spool(
    path: &Path,
    apply: impl FnOnce(Vec<HookEvent>) -> io::Result<()>,
) -> io::Result<()> {
    let mut f = match OpenOptions::new().read(true).write(true).open(path) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
        r => r?,
    };
    if f.metadata()?.len() == 0 {
        return Ok(());
    }
    let spool_lock = hook::spool_lock(path, &f)?;
    spool_lock.lock()?;
    let mut buf = Vec::new();
    f.read_to_end(&mut buf)?;
    let mut events = hook::read_spool(&buf);
    events.sort_by_key(|e| e.id.unix_ns);
    apply(events)?;
    f.set_len(0)
}

/// OWE-872: 루트 셸(`root`)과 전경 프로세스 그룹(`fg`)을 비교해, 루트 셸이 아닌 전경 프로세스가
/// 돌고 있으면 그 이름을 돌려준다. 루트 셸 자신이거나 끝난 세션·이름 불명이면 `None`.
pub(crate) fn foreground_process(
    root: u32,
    fg: Option<u32>,
    exited: bool,
    resolve: impl FnOnce(u32) -> Option<String>,
) -> Option<String> {
    if exited {
        return None;
    }
    let pid = fg.filter(|&pid| pid != 0 && pid != root)?;
    resolve(pid)
        .map(|name| name.trim().to_owned())
        .filter(|name| !name.is_empty() && name.len() <= krill_proto::control::MAX_AGENT_TEXT)
}

#[cfg(test)]
mod tests;
