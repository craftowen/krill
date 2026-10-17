//! 요청 시 Claude·Codex transcript 스캔(MK-082). control의 요청·응답 한 쌍을 재사용한다.
//! 스캔·새로고침 한 번당 디스크를 한 번 읽으며 감시·주기 작업은 없다.
use serde::{Deserialize, Serialize};

/// 지휘자 배정. `FEAT_CONTROL`과 함께 협상해야 한다.
pub const FEAT_AGENT_HISTORY: u64 = 1 << 50;
/// AI Vault 전문 검색·재개(`Action::Search`·`Action::ResumeVault`/`Response::Searched`, MK-181·OWE-735 지휘자 배정).
pub const FEAT_AI_VAULT: u64 = 1 << 32;
/// 한 응답은 프레임 상한(1MiB)보다 작다. 목록이 넘치면 `truncated`로 알린다.
pub const MAX_ENTRIES: usize = 100;
pub const MAX_TEXT: usize = 4096;
pub const MAX_FILTER_ITEMS: usize = 32;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum Agent {
    Claude,
    Codex,
}

impl Agent {
    #[must_use]
    pub fn profile(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Codex => "codex",
        }
    }
}

/// AI Vault 전문 검색·재개가 지원하는 에이전트 종류(MK-181).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum VaultAgent {
    Claude,
    Codex,
    Agy,
    OpenCode,
}

impl VaultAgent {
    pub const ALL: [Self; 4] = [Self::Claude, Self::Codex, Self::Agy, Self::OpenCode];

    #[must_use]
    pub fn profile(self) -> &'static str {
        match self {
            Self::Claude => "claude",
            Self::Codex => "codex",
            Self::Agy => "agy",
            Self::OpenCode => "opencode",
        }
    }

    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Claude => "Claude",
            Self::Codex => "Codex",
            Self::Agy => "Antigravity",
            Self::OpenCode => "OpenCode",
        }
    }

    #[must_use]
    pub fn parse(id: &str) -> Option<Self> {
        let trimmed = id.trim();
        if trimmed.eq_ignore_ascii_case("claude") {
            Some(Self::Claude)
        } else if trimmed.eq_ignore_ascii_case("codex") {
            Some(Self::Codex)
        } else if trimmed.eq_ignore_ascii_case("agy")
            || trimmed.eq_ignore_ascii_case("antigravity")
            || trimmed.eq_ignore_ascii_case("antigravity-cli")
        {
            Some(Self::Agy)
        } else if trimmed.eq_ignore_ascii_case("opencode")
            || trimmed.eq_ignore_ascii_case("open-code")
        {
            Some(Self::OpenCode)
        } else {
            None
        }
    }

    /// CLI 재개 명령 문자열을 만든다.
    #[must_use]
    pub fn resume_command(self, id: &str) -> String {
        match self {
            Self::Claude => format!("claude --resume {id}"),
            Self::Codex => format!("codex resume {id}"),
            Self::Agy => format!("agy --conversation {id}"),
            Self::OpenCode => format!("opencode --session {id}"),
        }
    }
}

impl From<Agent> for VaultAgent {
    fn from(agent: Agent) -> Self {
        match agent {
            Agent::Claude => Self::Claude,
            Agent::Codex => Self::Codex,
        }
    }
}

/// AI Vault 범위 필터(MK-181).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum VaultScope {
    #[default]
    All,
    Project,
    Workspace,
}

/// transcript 본문은 전송하지 않는다. cwd와 ID로 대화를 식별한다.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    pub agent: Agent,
    #[serde(deserialize_with = "text")]
    pub id: String,
    #[serde(deserialize_with = "text")]
    pub cwd: String,
    /// 파일 수정 시각(epoch 초), 최신순 정렬용.
    pub modified: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Snapshot {
    #[serde(deserialize_with = "entries")]
    pub entries: Vec<Entry>,
    /// 탐색·읽기·목록 예산에 걸려 전부 보지 못했다.
    pub truncated: bool,
    /// 읽을 수 없거나 지원하지 않는 형식인 파일·디렉터리가 있었다.
    pub skipped: bool,
}

/// AI Vault 전문 검색 요청(MK-181).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SearchQuery {
    #[serde(deserialize_with = "text")]
    pub query: String,
    /// 비어 있으면 모든 지원 에이전트(Claude·Codex·Agy·OpenCode)를 스캔한다.
    #[serde(deserialize_with = "vault_agents")]
    pub agents: Vec<VaultAgent>,
    /// 비어 있지 않으면 해당 경로(또는 하위 디렉터리)와 일치하는 cwd만 남긴다.
    #[serde(deserialize_with = "filter_paths")]
    pub paths: Vec<String>,
    /// 최대 반환 개수(0이면 `MAX_ENTRIES`).
    pub limit: u16,
}

/// AI Vault 검색 결과 한 건(MK-181).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VaultHit {
    pub agent: VaultAgent,
    #[serde(deserialize_with = "text")]
    pub id: String,
    #[serde(deserialize_with = "text")]
    pub cwd: String,
    pub modified: u64,
    #[serde(deserialize_with = "text")]
    pub title: String,
    #[serde(deserialize_with = "text")]
    pub summary: String,
    #[serde(deserialize_with = "text")]
    pub snippet: String,
    #[serde(deserialize_with = "text")]
    pub role: String,
    pub score: u32,
}

impl VaultHit {
    #[must_use]
    pub fn target(&self) -> VaultTarget {
        VaultTarget {
            agent: self.agent,
            id: self.id.clone(),
            cwd: self.cwd.clone(),
            modified: self.modified,
        }
    }
}

impl From<&Entry> for VaultHit {
    fn from(entry: &Entry) -> Self {
        Self {
            agent: entry.agent.into(),
            id: entry.id.clone(),
            cwd: entry.cwd.clone(),
            modified: entry.modified,
            title: String::new(),
            summary: String::new(),
            snippet: String::new(),
            role: String::new(),
            score: 0,
        }
    }
}

/// AI Vault 검색 응답(MK-181).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SearchSnapshot {
    #[serde(deserialize_with = "hits")]
    pub hits: Vec<VaultHit>,
    pub truncated: bool,
    pub skipped: bool,
}

/// AI Vault 멀티 에이전트 재개 대상(MK-181).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VaultTarget {
    pub agent: VaultAgent,
    #[serde(deserialize_with = "text")]
    pub id: String,
    #[serde(deserialize_with = "text")]
    pub cwd: String,
    pub modified: u64,
}

impl From<&Entry> for VaultTarget {
    fn from(entry: &Entry) -> Self {
        Self {
            agent: entry.agent.into(),
            id: entry.id.clone(),
            cwd: entry.cwd.clone(),
            modified: entry.modified,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Action {
    Scan,
    /// 기존 프로세스에는 손대지 않고 기본 프로필·현재 전역 권한으로 새 세션을 만든다.
    Resume(Entry),
    /// 멀티 에이전트 트랜스크립트 제목·요약·본문 전문 검색(MK-181).
    Search(SearchQuery),
    /// 멀티 에이전트 볼트 항목 재개(MK-181).
    ResumeVault(VaultTarget),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Response {
    Scanned(Snapshot),
    Resumed { session: crate::SessionId },
    Searched(SearchSnapshot),
}

fn text<'de, D: serde::Deserializer<'de>>(d: D) -> Result<String, D::Error> {
    struct Text;
    impl serde::de::Visitor<'_> for Text {
        type Value = String;
        fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("bounded history text")
        }
        fn visit_str<E: serde::de::Error>(self, s: &str) -> Result<String, E> {
            if s.len() > MAX_TEXT {
                return Err(E::custom("history text exceeds limit"));
            }
            Ok(s.to_owned())
        }
    }
    d.deserialize_str(Text)
}

fn entries<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Vec<Entry>, D::Error> {
    crate::control::bounded::<D, Entry, MAX_ENTRIES>(d)
}

fn hits<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Vec<VaultHit>, D::Error> {
    crate::control::bounded::<D, VaultHit, MAX_ENTRIES>(d)
}

fn vault_agents<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Vec<VaultAgent>, D::Error> {
    crate::control::bounded::<D, VaultAgent, MAX_FILTER_ITEMS>(d)
}

fn filter_paths<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Vec<String>, D::Error> {
    struct BoundedPath(String);
    impl<'de> serde::Deserialize<'de> for BoundedPath {
        fn deserialize<D2: serde::Deserializer<'de>>(d2: D2) -> Result<Self, D2::Error> {
            text(d2).map(Self)
        }
    }
    Ok(
        crate::control::bounded::<D, BoundedPath, MAX_FILTER_ITEMS>(d)?
            .into_iter()
            .map(|p| p.0)
            .collect(),
    )
}

#[cfg(test)]
mod tests;
