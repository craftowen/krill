//! 세션 생성·목록·닫기(OWE-486). [`crate::ipc`] v1 프레임 위에서, 협상한 feature에 [`FEAT_CONTROL`]이 있을 때만
//! 쓰는 kind다.
//!
//! # 흐름
//!
//! Hello 협상 뒤 클라이언트의 첫 프레임이 연결 종류를 정한다. [`crate::screen::kind::ATTACH`]면 화면·원본 스트림
//! ([`crate::screen`]), [`kind::REQUEST`]면 제어 연결이다. 제어 연결에서는 [`Request`] 하나에 [`Reply`] 하나가 순서대로
//! 온다(파이프라이닝 없음). 클라이언트가 닫을 때까지 여러 요청을 보낼 수 있다.
//!
//! # 의미
//!
//! - [`Request::Create`]: krilld가 holder에 `Spawn`을 보내 새 PTY 세션을 만든다. 성공하면 [`Reply::Created`].
//! - [`Request::List`]: 이 클라이언트가 다룰 수 있는 세션(세션 범위 권한, MK-036). 끝났지만 아직 닫지 않은 세션도 나온다.
//! - [`Request::Close`]: 돌고 있으면 holder가 종료 단계(SIGHUP → SIGTERM → SIGKILL, 탈출한 후손 포함)를 시작하고,
//!   끝나면 krilld가 세션을 지운다. 이미 끝났으면 바로 지운다. 종료를 기다리지 않고 [`Reply::Closed`]로 답한다.
//! - [`Request::NewWorktree`](MK-042): krilld의 git 작업 큐에서 워크트리를 만든다(수 초 걸릴 수 있다, 답은 끝난 뒤).
//!   레포의 `krill.toml` setup 스크립트를 이 내용으로 아직 신뢰한 적이 없으면 아무것도 만들지 않고
//!   [`Reply::SetupUntrusted`]로 답한다. 클라이언트는 사용자에게 스크립트를 보이고, 승인하면 받은 해시를
//!   [`Setup::Approve`]에 실어 다시 보낸다. 성공하면 [`Reply::WorktreeCreated`], 실패하면 [`Reply::WorktreeFailed`].
//! - [`Request::RemoveWorktree`](MK-043): [`FEAT_WORKTREE_REMOVE`]도 협상한 뒤에만 보낸다. dirty는 force가
//!   없으면 거부하고, 주 워크트리·locked는 항상 거부한다. 삭제 후 `branch -d`로 정리하며 보존된 브랜치는
//!   [`Reply::WorktreeRemoved`]의 경고로 알린다. prune은 Git의 기본 만료 기간을 따른다.
//! - [`Request::States`]: 다룰 수 있는 세션마다 에이전트 상태(MK-039, ARCHITECTURE §6 상태 모델). 근거가 없는 세션은
//!   [`AgentStatus::UNKNOWN`]. krilld가 디스크에 두므로 앱·krilld 재시작 뒤에도 같다.
//!   이 요청은 [`crate::hook::FEAT_HOOK`]도 협상했을 때만 보낸다(이전 데몬은 variant를 모른다).
//!
//! - [`Request::GitChanges`](MK-053): [`FEAT_GIT_CHANGES`]도 협상한 뒤에만 보낸다. 변경 파일과
//!   staged/unstaged numstat을 [`Reply::GitChanges`]로 반환한다. 경로 바이트와 목록은 [`crate::git`] 상한을
//!   따른다. 일회성 조회이며 자동 조회의 가시성·주기는 backend watcher owner가 관리한다.
//! - [`Request::Cold`](MK-032): [`FEAT_COLD`]도 협상한 뒤에만 보낸다. 프로세스가 사라진 세션(재부팅·holder와
//!   krilld 동시 소실)의 마지막 화면과 스크롤백을 디스크 세그먼트에서 재구성해 [`Reply::Cold`]로 답한다(읽기 전용).
//!   남은 기록이 없으면 [`Failure::NoSession`].
//!
//! - [`Request::AgentSettings`](OWE-488): [`FEAT_AGENTS`]를 협상한 뒤 전역 권한·프로필을 조회한다.
//!   권한 변경은 전체 범위의 interactive 클라이언트만 가능하고 `agents.toml`에 저장한다. 이후 생성하는
//!   세션부터 적용하며, 잘못된 설정 파일을 기본값으로 덮어쓰지 않는다.
//! - [`Request::AgentLaunch`]: 같은 feature로 세션 생성 당시 프로필·권한을 조회한다. 세션 범위를 따르며,
//!   krilld가 Spawn 전에 저장하므로 재시작·전역 토글 변경 후에도 YOLO 배지를 복원할 수 있다.
//!
//! - [`Request::Upgrade`](MK-062): [`FEAT_UPGRADE`]를 협상한 전체 범위 클라이언트만. 요청한 빌드가 krilld보다 새것일
//!   때만 holder를 새 바이너리로 exec 핸드오프하고, 다음 krilld 경로를 바꾼 뒤, PTY 리더를 멈추고 세그먼트를
//!   봉인한 다음 [`Upgraded::Started`]로 답하고 끝난다. holder가 새 krilld를 바로 띄워 세션을 넘긴다(세대 공존 없음).
//!   같거나 오래된 빌드면 아무것도 하지 않고 [`Upgraded::NotNewer`](다운그레이드 금지).
//!
//! # 호환
//!
//! [`crate::ipc`]의 규칙과 같다. enum variant와 struct 필드는 끝에만 붙인다. `control/tests.rs`의 고정 바이트가
//! 바뀌면 호환 위반이다.

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::SessionId;
use crate::holder::{MAX_SESSIONS, SESSION_LIMIT_ERRNO};
use crate::screen::Msg;

/// 이 모듈의 kind를 쓸 수 있다는 feature 비트.
pub const FEAT_CONTROL: u64 = 1 << 1;
/// 워크트리 생성 요청·응답 확장. [`FEAT_CONTROL`]과 함께 협상해야 한다.
pub const FEAT_WORKTREE: u64 = 1 << 4;
/// 워크트리 삭제 요청·응답 확장. [`FEAT_CONTROL`]과 함께 협상해야 한다.
pub const FEAT_WORKTREE_REMOVE: u64 = 1 << 5;
/// 변경 파일·numstat 조회. [`FEAT_CONTROL`]과 함께 협상해야 한다.
pub const FEAT_GIT_CHANGES: u64 = 1 << 6;
/// CLI 화면 캡처·PTY 크기. 기존 REQUEST/REPLY에서만 사용한다(MK-045).
pub const FEAT_CLI: u64 = 1 << 10;
/// 세션 상태·원본 출력 정지 시간 조회(MK-047). `FEAT_CONTROL`과 함께 협상한다.
pub const FEAT_WAIT_STATUS: u64 = 1 << 11;
/// Read-only plain capture with an unchanged stamp; requires `FEAT_CLI` and `FEAT_CONTROL`.
pub const FEAT_CAPTURE_SINCE: u64 = 1 << 23;
/// MK-046 repository/worktree metadata, Request 10 / Reply 14.
pub const FEAT_SIDEBAR: u64 = 1 << 14;
/// 에이전트 전역 권한·프로필과 세션별 실행 권한 조회(OWE-488). 기존 Create는 그대로 쓴다.
pub const FEAT_AGENTS: u64 = 1 << 31;
/// 에이전트 세션 포크(`Program::Agent { id: "fork:<spec>" }`, OWE-736, 지휘자 배정). [`FEAT_CONTROL`]과 함께 협상한다.
pub const FEAT_SESSION_FORK: u64 = 1 << 34;
/// krilld 업그레이드 요청(MK-062, 지휘자 배정). [`Request::Upgrade`]·[`Reply::Upgrade`] 한 쌍.
pub const FEAT_UPGRADE: u64 = 1 << 36;
/// 설정 응답의 프로필 수 상한.
pub const MAX_AGENT_PROFILES: usize = 256;
/// 프로필 id·표시 이름의 UTF-8 바이트 상한.
pub const MAX_AGENT_TEXT: usize = 4096;
/// 콜드 복원 화면 조회(MK-032). `FEAT_CONTROL`과 함께 협상한다.
pub const FEAT_COLD: u64 = 1 << 18;
/// 세션 재개 요청(MK-052, 지휘자 배정). [`FEAT_CONTROL`]과 함께 협상한다.
pub const FEAT_RESUME: u64 = 1 << 42;
/// 재개 전 세그먼트를 새 세션의 HISTORY 페이지로 제공한다(지휘자 배정).
pub const FEAT_RESUME_HISTORY: u64 = 1 << 30;
/// `FSEvents` 변경 감시·가시성 제어(OWE-585, 지휘자 배정). [`Request::GitWatch`]·[`Reply::GitWatch`] 한 쌍.
pub const FEAT_GIT_WATCH: u64 = 1 << 44;
/// Hibernation 표식 조회(MK-081). `FEAT_CONTROL`과 함께 협상한다.
pub const FEAT_HIBERNATION: u64 = 1 << 51;
/// MK-095 speculative worktree preparation (coordinator allocation).
pub const FEAT_WORKTREE_PREPARE: u64 = 1 << 52;
/// Git index 변경·커밋(MK-088, 지휘자 배정). `FEAT_CONTROL`과 함께 협상한다.
pub const FEAT_GIT_EDIT: u64 = 1 << 48;
pub use crate::ipc::FEAT_GIT_SYNC;
/// 세션별 사용 계정 꼬리(OWE-707, MK-147). [`Reply::Sessions`]의 [`Info::account`]를
/// 받을 수 있다는 표시. 없는 피어에게는 꼬리를 비워 보낸다.
pub const FEAT_SESSION_ACCOUNT: u64 = 1 << 58;
/// 세션 스크롤백 지우기 요청(OWE-873b, 지휘자 배정). [`Request::ClearScrollback`]·[`Reply::ScrollbackCleared`] 한 쌍.
pub const FEAT_CLEAR_SCROLLBACK: u64 = 1 << 47;
/// 원격 파일 탐색·열기·원자적 저장·Quick Open·루프백 포트 포워딩. [`Request::RemoteFs`]·[`Reply::RemoteFs`] 한 쌍.
pub const FEAT_REMOTE_FS: u64 = 1 << 27;
/// 캡처 한 번의 최대 바이트. 넘으면 잘라내지 않고 `BadRequest`로 거부한다.
pub const MAX_CAPTURE: usize = 512 * 1024;

/// CLI 캡처 표현. ANSI는 호스트 터미널 초기 화면 복원용이다.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CaptureFormat {
    Plain,
    Ansi,
}

/// 이 모듈의 kind. 값은 영구다.
pub mod kind {
    /// 클라이언트 → krilld: [`super::Request`].
    pub const REQUEST: u8 = 0x30;
    /// krilld → 클라이언트: [`super::Reply`].
    pub const REPLY: u8 = 0x31;
}

/// 세션에서 띄울 것. variant는 끝에만 붙인다.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Program {
    /// passwd의 로그인 셸을 `-l`로(Terminal.app 기본, MK-034).
    Login,
    /// 실행 파일과 인자를 그대로.
    Custom {
        program: Vec<u8>,
        args: Vec<Vec<u8>>,
    },
    /// 에이전트 프로필(MK-037, `agents.toml`의 키). 권한 모드는 `agents.toml`의 전역 토글을 따른다.
    Agent { id: String },
}

/// 새 세션.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Create {
    pub program: Program,
    /// 작업 디렉터리. 비어 있으면 holder의 작업 디렉터리를 물려받는다.
    pub cwd: Vec<u8>,
    pub cols: u16,
    pub rows: u16,
    /// OWE-712: 새 Agent 세션 전용. 기능 비트 61 협상 필수.
    #[serde(default)]
    pub account: Option<String>,
}

/// [`kind::REQUEST`]. variant는 끝에만 붙인다.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Request {
    Create(Create),
    List,
    Close {
        session: SessionId,
    },
    States,
    NewWorktree(NewWorktree),
    RemoveWorktree(RemoveWorktree),
    /// 일회성 조회. 자동 감시·창 가시성은 backend watcher API에서 관리한다.
    GitChanges {
        repo: Vec<u8>,
    },
    /// `FEAT_CLI` 필요. 화면과 원본 위치를 같은 잠금으로 캡처한다.
    Capture {
        session: SessionId,
        format: CaptureFormat,
    },
    /// `FEAT_CLI` 필요. `1..=MAX_COLS` × `1..=MAX_LINES`, PTY와 엔진을 함께 바꾼다.
    Resize {
        session: SessionId,
        cols: u16,
        rows: u16,
    },
    /// `FEAT_WAIT_STATUS` 필요. 범위 내 세션 하나의 상태와 출력 정지 시간을 조회한다.
    WaitStatus {
        session: SessionId,
    },
    /// `FEAT_SIDEBAR` required; scoped session cwd and containing repositories.
    Sidebar,
    /// `FEAT_AGENTS` 필요. None은 조회, Some은 전역 권한 저장(새 세션에만 적용).
    AgentSettings {
        permission: Option<crate::agent::Permission>,
    },
    /// `FEAT_AGENTS` 필요. 현재 전역 권한과 무관한 세션 생성 당시의 값.
    AgentLaunch {
        session: SessionId,
    },
    /// `FEAT_UPGRADE` 필요(MK-062).
    Upgrade(Upgrade),
    /// `FEAT_COLD` 필요. 디스크에 남은 세션 기록의 마지막 화면과 스크롤백.
    Cold {
        session: SessionId,
    },
    /// Resource Manager(MK-071). 재시작은 전체 범위 interactive만.
    Resources(crate::resources::Action),
    /// `FEAT_RESUME` 필요(MK-052). 사라졌거나 끝난 세션의 에이전트 대화를 이어받아 새 세션을 만든다.
    Resume {
        session: SessionId,
        create: Create,
    },
    /// `FEAT_GIT_WATCH` 필요(OWE-585). 저장소 `FSEvents` 감시 조회·가시성 제어.
    GitWatch(crate::git::GitWatchRequest),
    /// 잠든 세션이면 자동 재개한다. 아니면 None으로 답한다.
    Hibernation {
        session: SessionId,
        create: Create,
    },
    /// `FEAT_WORKTREE_PREPARE` required. Repeating a token queries its preparation.
    PrepareWorktree(PrepareWorktree),
    /// 요청 시 transcript 스캔·새 세션 재개(MK-082).
    AgentHistory(crate::agent_history::Action),
    /// 전체 범위 interactive만. 커밋 확인 대화상자는 앱에서 처리한다.
    GitEdit {
        repo: Vec<u8>,
        edit: crate::git::Edit,
    },
    /// `FEAT_SESSION_ACCOUNT` 필요(OWE-707). 범위 내 세션의 실행 당시 계정 표시용 목록.
    /// `List`와 함께 읽어 탭·사이드바 배지를 그린다. PTY에는 손대지 않는다.
    SessionAccounts,
    /// `FEAT_CLEAR_SCROLLBACK` 필요(OWE-873b). 지정한 세션의 핫 스크롤백과 콜드 히스토리 기준선을 지운다.
    /// 세션·PTY·현재 보이는 화면은 건드리지 않는다.
    ClearScrollback {
        session: SessionId,
    },
    /// `FEAT_CLI` 필요(MK-225). 제어 연결 한 번으로 세션 PTY에 입력을 전달한다.
    /// 세션이 bracketed paste 모드면 본문을 `\x1b[200~...\x1b[201~`로 감싸고 `submit`을 별도 `\r`로 보낸다.
    SendInput {
        session: SessionId,
        #[serde(deserialize_with = "input_bytes")]
        bytes: Vec<u8>,
        submit: bool,
        wait_submit: bool,
    },
    /// 멀티에이전트 오케스트레이션 제어·대기 요청(MK-227).
    Orchestration(crate::orchestration::OrchAction),
    /// Appended extension: a stamp is scoped to this session and daemon incarnation.
    /// An equal stamp avoids screen composition/encoding. It is not a raw replay cursor.
    CaptureSince {
        session: SessionId,
        since: Option<CaptureStamp>,
    },
    /// `FEAT_REMOTE_FS` 필요. 원격 파일 탐색·읽기·원자적 저장·Quick Open·루프백 포트 포워딩.
    RemoteFs(crate::remote_fs::RemoteFsRequest),
}

/// A dialog owns one token; cancellation also applies if it arrives before preparation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PrepareWorktree {
    pub token: u64,
    #[serde(deserialize_with = "sidebar_path")]
    pub repo: Vec<u8>,
    #[serde(deserialize_with = "optional_preparation_base")]
    pub base: Option<String>,
    pub cancel: bool,
}

/// Preparation never runs setup or copies ignored includes. Creation still validates both.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum WorktreePreparation {
    Preparing,
    Ready,
    Busy,
    Cancelled,
    Skipped,
    Failed(String),
}

fn optional_preparation_base<'de, D: serde::Deserializer<'de>>(
    d: D,
) -> Result<Option<String>, D::Error> {
    let value = Option::<String>::deserialize(d)?;
    if value.as_ref().is_some_and(|s| s.len() > MAX_AGENT_TEXT) {
        return Err(serde::de::Error::custom(
            "preparation base exceeds byte limit",
        ));
    }
    Ok(value)
}

/// 이 빌드의 holder·krilld로 교체해 달라는 요청. 경로는 krilld가 그대로 holder에 넘긴다.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Upgrade {
    /// 요청자 빌드(`crate::BUILD`). krilld 빌드보다 새것이어야 한다([`is_newer`]).
    #[serde(deserialize_with = "agent_text")]
    pub build: String,
    #[serde(deserialize_with = "sidebar_path")]
    pub holder: Vec<u8>,
    #[serde(deserialize_with = "sidebar_path")]
    pub krilld: Vec<u8>,
}

/// [`Request::Upgrade`]의 결과. variant는 끝에만 붙인다.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Upgraded {
    /// 교체를 시작했다. 이 답 뒤 krilld는 연결을 닫고 끝난다.
    Started,
    /// 요청 빌드가 krilld보다 새것이 아니다. 아무것도 바꾸지 않았다.
    NotNewer,
}

/// `candidate`가 `current`보다 새 빌드인가. 버전이 같으면 개발 빌드(`+dev.<소스 해시>`)의 해시 차이도 교체한다.
/// 낮은 버전은 개발 빌드라도 거절한다. 버전을 읽지 못하면 `None`.
/// 프리릴리스(`-beta.1` 등)는 semver 순서로 비교한다. 안정 버전만 쓰면 예전과
/// 결과가 같다(AS-11).
pub fn is_newer(candidate: &str, current: &str) -> Option<bool> {
    fn parse(v: &str) -> Option<([u64; 3], Option<&str>)> {
        let (without_build, _) = v.split_once('+').map_or((v, None), |(c, b)| (c, Some(b)));
        let (core, pre) = without_build
            .split_once('-')
            .map_or((without_build, None), |(c, p)| (c, Some(p)));
        let mut it = core.split('.').map(|n| n.parse().ok());
        let parsed = [it.next()??, it.next()??, it.next()??];
        it.next().is_none().then_some((parsed, pre))
    }
    fn pre_is_newer(candidate: Option<&str>, current: Option<&str>) -> Option<bool> {
        match (candidate, current) {
            (None, None) => None,
            // 릴리스가 프리릴리스보다 새것이다.
            (None, Some(_)) => Some(true),
            (Some(_), None) => Some(false),
            (Some(a), Some(b)) => {
                if a == b {
                    return None;
                }
                Some(compare_pre(a, b).is_gt())
            }
        }
    }
    /// semver §11 식별자 비교: 숫자 식별자는 수로, 숫자와 비숫자는 숫자가
    /// 작게, 앞부분이 같으면 식별자가 많은 쪽이 크다.
    fn compare_pre(a: &str, b: &str) -> std::cmp::Ordering {
        use std::cmp::Ordering;
        let mut ai = a.split('.');
        let mut bi = b.split('.');
        loop {
            match (ai.next(), bi.next()) {
                (Some(x), Some(y)) => {
                    let ord = match (x.parse::<u64>(), y.parse::<u64>()) {
                        (Ok(xn), Ok(yn)) => xn.cmp(&yn),
                        (Ok(_), Err(_)) => Ordering::Less,
                        (Err(_), Ok(_)) => Ordering::Greater,
                        (Err(_), Err(_)) => x.cmp(y),
                    };
                    if ord != Ordering::Equal {
                        return ord;
                    }
                }
                (Some(_), None) => return Ordering::Greater,
                (None, Some(_)) => return Ordering::Less,
                (None, None) => return Ordering::Equal,
            }
        }
    }
    let (candidate_version, candidate_pre) = parse(candidate)?;
    let (current_version, current_pre) = parse(current)?;
    if candidate_version != current_version {
        return Some(candidate_version > current_version);
    }
    if let Some(newer) = pre_is_newer(candidate_pre, current_pre) {
        return Some(newer);
    }
    Some(
        candidate
            .split_once("+dev.")
            .is_some_and(|(_, hash)| !hash.is_empty())
            && candidate != current,
    )
}

/// 앱의 에이전트 선택 목록. 실행 argv·env는 krilld에서만 만든다.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentProfile {
    #[serde(deserialize_with = "agent_text")]
    pub id: String,
    #[serde(deserialize_with = "agent_text")]
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentSettings {
    pub permission: crate::agent::Permission,
    #[serde(deserialize_with = "agent_profiles")]
    pub profiles: Vec<AgentProfile>,
}

/// 세션 생성 시 고정한 에이전트·권한. 데몬 재시작 뒤에도 배지는 이 값을 따른다.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentLaunch {
    #[serde(deserialize_with = "agent_text")]
    pub profile: String,
    pub permission: crate::agent::Permission,
}

fn agent_profiles<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Vec<AgentProfile>, D::Error> {
    bounded::<D, AgentProfile, MAX_AGENT_PROFILES>(d)
}

fn agent_text<'de, D: serde::Deserializer<'de>>(d: D) -> Result<String, D::Error> {
    struct Text;
    impl serde::de::Visitor<'_> for Text {
        type Value = String;
        fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str("bounded agent text")
        }
        fn visit_str<E: serde::de::Error>(self, text: &str) -> Result<String, E> {
            if text.len() > MAX_AGENT_TEXT {
                return Err(E::custom("agent text exceeds limit"));
            }
            Ok(text.to_owned())
        }
    }
    d.deserialize_str(Text)
}

/// 에이전트 상태 6가지(ARCHITECTURE §6). `idle`은 상태가 아니라 파생 조건이다. variant는 끝에만 붙인다.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AgentState {
    Working,
    /// 사람의 답(권한 승인·질문)을 기다린다.
    Waiting,
    /// 화면 규칙으로 잡은 승인 메뉴·trust 프롬프트에 멈췄다.
    Blocked,
    /// 턴이 끝나 프롬프트에 있다.
    Done,
    Exited,
    Unknown,
}

/// 상태의 근거. 감지 우선순위는 hook > 프로세스 > 제목 > 화면. variant는 끝에만 붙인다.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Source {
    Hook,
    Process,
    Title,
    Screen,
}

/// 근거의 신뢰도. variant는 끝에만 붙인다.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Confidence {
    Low,
    Medium,
    High,
}

/// 세션 하나의 에이전트 상태. 필드는 끝에만 붙인다.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentStatus {
    pub state: AgentState,
    /// 근거가 없으면 `None`([`AgentState::Unknown`]).
    pub source: Option<Source>,
    pub confidence: Confidence,
    /// 마지막 hook의 에이전트 이름(`krill hook <agent>`). 모르면 빈 문자열.
    pub agent: String,
    /// 지금 상태가 된 시각(UNIX epoch 나노초, hook 이벤트면 hook 프로세스 시작 시각). 모르면 0.
    pub since_unix_ns: u64,
    /// OWE-872: 셸이 아닌 전경 프로세스 이름. 루트 셸이거나 없으면 `None`.
    /// [`Reply::States`] 프레임 끝 꼬리로만 오가 구버전(`Record`·v1 fixture)과 상호 호환한다.
    #[serde(default, skip)]
    pub foreground: Option<String>,
    /// 터미널 제목(OSC 0·2)에서 상태 글리프를 뗀 표시용 제목([`display_title`]). 없으면 `None`.
    /// `foreground` 꼬리 뒤 두 번째 꼬리로만 오가 구버전과 상호 호환한다.
    #[serde(default, skip)]
    pub title: Option<String>,
    /// 지금 돌고 있는 서브에이전트(Claude `SubagentStart`~`SubagentStop`). 최대 [`MAX_SUBAGENTS`]개.
    /// `title` 꼬리 뒤 세 번째 꼬리로만 오가 구버전과 상호 호환한다.
    #[serde(default, skip)]
    pub subagents: Vec<Subagent>,
}

/// 세션 안에서 돌고 있는 서브에이전트 하나. 필드는 끝에만 붙인다.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Subagent {
    /// `agent_type`(예: `general-purpose`). 모르면 빈 문자열. 최대 [`MAX_AGENT_TEXT`] 바이트.
    pub kind: String,
    /// 부모가 Agent 도구에 준 `description`. 모르면 빈 문자열. 최대 [`MAX_TITLE_TEXT`] 바이트.
    pub description: String,
    /// 시작 시각(UNIX epoch 나노초).
    pub since_unix_ns: u64,
}

/// [`AgentStatus::subagents`] 상한.
pub const MAX_SUBAGENTS: usize = 16;

/// 에이전트가 터미널 제목 앞에 붙이는 상태 글리프(Claude Code `✳`·점자 스피너, Gemini `✦`·`◇` 등)와 제어 문자를
/// 떼어 낸 제목. 바뀌는 스피너가 목록을 매번 다시 그리지 않게 하고, 남는 게 없으면 `None`이다.
#[must_use]
pub fn display_title(raw: &str) -> Option<String> {
    let clean = crate::screen::sanitize_title_max(raw, MAX_TITLE_TEXT);
    let trimmed = clean
        .trim_start_matches(|c: char| {
            c.is_whitespace()
                || matches!(c, '\u{2800}'..='\u{28ff}' | '\u{2700}'..='\u{27bf}' | '\u{25a0}'..='\u{25ff}' | '·' | '*')
        })
        .trim_end();
    (!trimmed.is_empty()).then(|| trimmed.to_owned())
}

/// [`AgentStatus::title`] UTF-8 바이트 상한.
pub const MAX_TITLE_TEXT: usize = 256;

impl AgentStatus {
    /// MK-049 shared UI badge for inferred title/screen evidence.
    pub fn badge(&self) -> Option<&'static str> {
        matches!(self.source, Some(Source::Title | Source::Screen)).then_some("추정")
    }

    /// 근거가 하나도 없는 세션.
    pub const UNKNOWN: Self = Self {
        state: AgentState::Unknown,
        source: None,
        confidence: Confidence::Low,
        agent: String::new(),
        since_unix_ns: 0,
        foreground: None,
        title: None,
        subagents: Vec::new(),
    };
}

/// [`Reply::States`]의 한 줄.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionStatus {
    pub session: SessionId,
    pub status: AgentStatus,
}

/// MK-046 paths use OS bytes, including non-UTF-8 names.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SidebarData {
    #[serde(deserialize_with = "sidebar_sessions")]
    pub sessions: Vec<SessionDirectory>,
    #[serde(deserialize_with = "sidebar_repositories")]
    pub repositories: Vec<RepositoryTrees>,
    /// 워크트리 경로별 현재 브랜치(`refs/heads/` 제거). 와이어 호환을 위해 [`Reply::Sidebar`] 꼬리 바이트로만 인코딩한다.
    #[serde(default, skip)]
    pub branches: Vec<(Vec<u8>, String)>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionDirectory {
    pub session: SessionId,
    /// Empty if unavailable (exited before the first query, for example).
    #[serde(deserialize_with = "sidebar_path")]
    pub cwd: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepositoryTrees {
    #[serde(deserialize_with = "sidebar_path")]
    pub repo: Vec<u8>,
    #[serde(deserialize_with = "sidebar_trees")]
    pub worktrees: Vec<Vec<u8>>,
}

#[derive(Deserialize)]
struct Branch(
    #[serde(deserialize_with = "sidebar_path")] Vec<u8>,
    #[serde(deserialize_with = "branch_text")] String,
);
#[derive(Deserialize)]
#[serde(transparent)]
struct Branches(#[serde(deserialize_with = "bounded::<_, Branch, 512>")] Vec<Branch>);
fn branch_text<'de, D: serde::Deserializer<'de>>(d: D) -> Result<String, D::Error> {
    let text = <&str>::deserialize(d)?;
    if text.is_empty() || text.len() > MAX_TITLE_TEXT {
        return Err(serde::de::Error::custom("branch text exceeds limit"));
    }
    Ok(text.to_owned())
}

pub(crate) fn bounded<'de, D, T, const MAX: usize>(d: D) -> Result<Vec<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    struct Items<T, const MAX: usize>(std::marker::PhantomData<T>);
    impl<'de, T: Deserialize<'de>, const MAX: usize> serde::de::Visitor<'de> for Items<T, MAX> {
        type Value = Vec<T>;
        fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str("bounded sidebar metadata")
        }
        fn visit_seq<A: serde::de::SeqAccess<'de>>(
            self,
            mut seq: A,
        ) -> Result<Self::Value, A::Error> {
            use serde::de::Error as _;
            if seq.size_hint().is_some_and(|n| n > MAX) {
                return Err(A::Error::custom("sidebar collection exceeds limit"));
            }
            let mut items = Vec::new();
            while let Some(item) = seq.next_element()? {
                if items.len() >= MAX {
                    return Err(A::Error::custom("sidebar collection exceeds limit"));
                }
                items.push(item);
            }
            Ok(items)
        }
    }
    d.deserialize_seq(Items::<T, MAX>(std::marker::PhantomData))
}

fn sidebar_sessions<'de, D: serde::Deserializer<'de>>(
    d: D,
) -> Result<Vec<SessionDirectory>, D::Error> {
    bounded::<D, SessionDirectory, MAX_SESSIONS>(d)
}
fn sidebar_repositories<'de, D: serde::Deserializer<'de>>(
    d: D,
) -> Result<Vec<RepositoryTrees>, D::Error> {
    bounded::<D, RepositoryTrees, MAX_SESSIONS>(d)
}
fn sidebar_path<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Vec<u8>, D::Error> {
    bounded::<D, u8, 4096>(d)
}
fn sidebar_trees<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Vec<Vec<u8>>, D::Error> {
    #[derive(Deserialize)]
    #[serde(transparent)]
    struct Path(#[serde(deserialize_with = "sidebar_path")] Vec<u8>);
    Ok(bounded::<D, Path, 512>(d)?
        .into_iter()
        .map(|p| p.0)
        .collect())
}

/// 워크트리 생성 뒤 setup 스크립트 처리. variant는 끝에만 붙인다.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Setup {
    /// 신뢰한 내용이면 돌리고, 아니면 [`Reply::SetupUntrusted`].
    Ask,
    /// 돌리지 않는다.
    Skip,
    /// 사용자가 [`Reply::SetupUntrusted`]의 해시를 승인했다. 내용이 그 사이 바뀌었으면 다시 묻는다.
    Approve(Vec<u8>),
}

/// 새 워크트리. 경로는 `<워크트리 루트>/<레포 이름>/<name>`, 브랜치는 `name`. 겹치면 `-2`부터 붙인다.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NewWorktree {
    /// 레포(주 워크트리) 경로.
    pub repo: Vec<u8>,
    /// 사용자가 입력한 이름(한글 가능). 공백은 `-`로 바꾼다.
    pub name: String,
    /// 기준 ref. `None`이면 레포 설정 → origin/HEAD → origin/main → origin/master → main → master → HEAD.
    pub base: Option<String>,
    pub setup: Setup,
}

/// 만든 워크트리.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Worktree {
    pub path: Vec<u8>,
    pub branch: String,
    pub base: String,
    /// `include`에서 복사한 경로.
    pub copied: Vec<String>,
    /// 생성은 됐지만 사용자에게 알릴 것(복사를 거부한 include 경로와 이유, setup 세션 실패 등).
    pub warnings: Vec<String>,
    /// setup 스크립트를 돌리는 세션.
    pub setup: Option<SessionId>,
}

/// 삭제할 링크된 워크트리. 주 워크트리와 잠긴 워크트리는 force와 무관하게 거부한다.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoveWorktree {
    pub repo: Vec<u8>,
    pub path: Vec<u8>,
    /// 변경·미추적·무시된 파일을 버리는 것에 대한 명시적 승인. 브랜치 강제 삭제에는 쓰지 않는다.
    pub force: bool,
}

/// 워크트리 삭제 결과. 브랜치 정리 실패는 삭제 성공과 구별해 경고로 보고한다.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemovedWorktree {
    pub branch: Option<String>,
    pub branch_deleted: bool,
    pub warnings: Vec<String>,
}

/// 세션 실행에 쓴 계정(OWE-707, MK-147). OWE-698이 `StoredLaunch`에 기록한
/// 실행 당시 계정 중 UI 배지에 필요한 표시용만 싣는다. `dir` 같은 경로·비밀값은 싣지 않는다.
/// [`Reply::SessionAccounts`] 원소로 오가며, 필드는 끝에만 붙인다.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionAccount {
    pub session: SessionId,
    /// `accounts.toml`의 계정 id. 모르면 빈 값이다(Orca 감지 계정 등).
    #[serde(default, deserialize_with = "session_account_text")]
    pub account_id: String,
    /// 계정 카드 표시 이름. 툴팁은 이 값, 배지는 UI가 축약한다.
    /// 구 기록(JSON)은 이 필드 없이 읽히고 빈 값으로 본다.
    #[serde(default, deserialize_with = "session_account_text")]
    pub label: String,
}

fn session_account_text<'de, D: serde::Deserializer<'de>>(d: D) -> Result<String, D::Error> {
    agent_text(d)
}

/// 세션 하나의 요약.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Info {
    pub session: SessionId,
    /// 세션 루트 프로세스(holder가 띄운 셸·에이전트).
    pub pid: u32,
    /// 끝났으면 `waitpid` 원시 상태 또는 [`crate::holder::STATUS_UNKNOWN`].
    pub status: Option<i32>,
}

/// 요청 실패. variant는 끝에만 붙인다.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Failure {
    /// krilld에 holder 링크가 없다(holder 없이 떴거나 holder가 사라짐). 새 세션을 만들 수 없다.
    NoHolder,
    /// holder가 보관하는 세션이 [`MAX_SESSIONS`]개다(holder `Spawn`의 `EPROCLIM`).
    SessionLimit,
    NoSession,
    /// 세션 범위 권한 밖(MK-036). 감사 로그에 남는다.
    Denied,
    /// 없는 에이전트 프로필이거나 `agents.toml`을 읽지 못했다.
    UnknownAgent,
    /// 요청을 읽지 못했다(이 krilld가 모르는 variant 등).
    BadRequest,
    /// 그 밖의 OS 오류. openpty·fork·exec 실패 등. 값은 이 프로세스 OS의 errno다. 와이어 표현([`wire_errno`])이
    /// Linux 원격 호스트의 번호를 구분해, Mac 앱이 틀린 문구를 보이지 않게 한다.
    Os(#[serde(with = "wire_errno")] i32),
}

/// [`Failure::Os`]의 와이어 표현. macOS는 자기 errno 그대로다. Linux는 두 OS 번호가 같은 1~34만 그대로 보내고,
/// 그 밖은 `LINUX_TAG + errno`로 보낸다 — 번호가 겹쳐도(Linux ETIME 62 = macOS ELOOP 62) 같은 OS끼리는 늘 원래 값으로
/// 돌아오고, 받는 macOS는 아는 것만 자기 번호로 옮긴다(모르는 값은 "Unknown error 100xx"로 남아 틀린 문구를 피한다).
mod wire_errno {
    use serde::{Deserialize as _, Deserializer, Serializer};

    /// Linux errno 표식. 실제 errno는 이보다 훨씬 작다.
    const LINUX_TAG: i32 = 10_000;
    /// 1~34는 두 OS가 같다.
    #[cfg(target_os = "linux")]
    const SHARED_MAX: i32 = 34;
    /// (Linux, macOS) 번호가 다른 errno 중 spawn·IO·소켓에서 나올 만한 것.
    const PAIRS: &[(i32, i32)] = &[
        (35, 11),  // EDEADLK
        (36, 63),  // ENAMETOOLONG
        (38, 78),  // ENOSYS
        (39, 66),  // ENOTEMPTY
        (40, 62),  // ELOOP
        (95, 45),  // EOPNOTSUPP
        (98, 48),  // EADDRINUSE
        (104, 54), // ECONNRESET
        (110, 60), // ETIMEDOUT
        (111, 61), // ECONNREFUSED
    ];
    /// Linux EAGAIN(11)은 1~34 안이라 그대로 가지만 macOS EAGAIN은 35다.
    #[cfg(target_os = "linux")]
    const LINUX_EAGAIN: i32 = 11;
    #[cfg(target_os = "linux")]
    const MACOS_EAGAIN: i32 = 35;

    #[cfg(target_os = "linux")]
    pub(super) fn to_wire(local: i32) -> i32 {
        if (1..=SHARED_MAX).contains(&local) {
            local
        } else {
            LINUX_TAG.saturating_add(local)
        }
    }
    #[cfg(target_os = "linux")]
    pub(super) fn from_wire(wire: i32) -> i32 {
        if wire > LINUX_TAG {
            return wire.saturating_sub(LINUX_TAG);
        }
        // 1~34는 Linux 피어가 보낸 같은 번호다. 그보다 큰 값은 macOS 피어(원격 Mac 호스트)의 번호라 아는 것만 옮긴다.
        if wire <= SHARED_MAX {
            return wire;
        }
        if wire == MACOS_EAGAIN {
            return LINUX_EAGAIN;
        }
        PAIRS
            .iter()
            .find(|p| p.1 == wire && p.1 > SHARED_MAX)
            .map_or(wire, |p| p.0)
    }
    #[cfg(not(target_os = "linux"))]
    pub(super) fn to_wire(local: i32) -> i32 {
        local
    }
    #[cfg(not(target_os = "linux"))]
    pub(super) fn from_wire(wire: i32) -> i32 {
        let Some(linux) = wire.checked_sub(LINUX_TAG).filter(|e| *e > 0) else {
            // macOS 피어, 또는 Linux의 1~34(같은 번호). 단 Linux EAGAIN(11)은 macOS EDEADLK(11)과 겹쳐 구분할 수 없어 그대로 둔다.
            return wire;
        };
        PAIRS.iter().find(|p| p.0 == linux).map_or(wire, |p| p.1)
    }

    #[expect(
        clippy::trivially_copy_pass_by_ref,
        reason = "serde with 계약이 &T를 요구한다"
    )]
    pub(super) fn serialize<S: Serializer>(errno: &i32, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_i32(to_wire(*errno))
    }

    pub(super) fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<i32, D::Error> {
        i32::deserialize(d).map(from_wire)
    }
}

impl Failure {
    /// holder `Failed{errno}`를 사용자에게 보일 실패로 바꾼다.
    pub fn from_holder_errno(errno: i32) -> Self {
        if errno == SESSION_LIMIT_ERRNO {
            Self::SessionLimit
        } else {
            Self::Os(errno)
        }
    }
}

impl fmt::Display for Failure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoHolder => f.write_str("krill-holder is not connected; cannot create sessions"),
            Self::SessionLimit => write!(
                f,
                "session limit reached ({MAX_SESSIONS} sessions); close a session first"
            ),
            Self::NoSession => f.write_str("no such session"),
            Self::Denied => f.write_str("not allowed from inside another session"),
            Self::UnknownAgent => f.write_str("unknown agent profile"),
            Self::BadRequest => f.write_str("bad request"),
            Self::Os(e) => write!(f, "{}", std::io::Error::from_raw_os_error(*e)),
        }
    }
}

/// [`kind::REPLY`]. variant는 끝에만 붙인다.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Reply {
    Created {
        session: SessionId,
        pid: u32,
    },
    Sessions(Vec<Info>),
    Closed,
    Failed(Failure),
    States(Vec<SessionStatus>),
    WorktreeCreated(Worktree),
    /// setup 스크립트를 이 내용으로 신뢰한 적이 없다. `hash`를 [`Setup::Approve`]로 돌려주면 승인이다.
    SetupUntrusted {
        script: String,
        hash: Vec<u8>,
    },
    /// 워크트리를 만들거나 삭제하지 못했다(사람이 읽을 이유).
    WorktreeFailed(String),
    WorktreeRemoved(RemovedWorktree),
    GitChanges(crate::git::Changes),
    GitChangesFailed(String),
    /// `FEAT_CLI` 필요. ANSI 복원 뒤 `Raw { from: seq }`로 잇는다. 미완성 UTF-8·제어열과
    /// sync 버퍼는 화면에 아직 없으므로 seq에서 제외하고 원본 스트림으로 보낸다.
    Captured {
        seq: crate::screen::Seq,
        #[serde(deserialize_with = "capture_bytes")]
        bytes: Vec<u8>,
    },
    Resized,
    /// `FEAT_WAIT_STATUS` 필요. idle은 별도 상태가 아니라 CLI에서 파생한다.
    WaitStatus {
        state: AgentState,
        /// 마지막 원본 PTY 수신 뒤 단조 시계 경과 시간. 재기동하면 0부터 센다.
        quiet_ms: u64,
    },
    Sidebar(SidebarData),
    /// `FEAT_AGENTS` 필요.
    AgentSettings(AgentSettings),
    /// `FEAT_AGENTS` 필요. 에이전트가 아닌 세션·기록 없는 이전 세션은 None.
    AgentLaunch(Option<AgentLaunch>),
    /// `FEAT_UPGRADE` 필요.
    Upgrade(Upgraded),
    /// `FEAT_COLD` 필요.
    Cold(Cold),
    /// MK-071. [`Request::Resources`]의 두 동작 모두 성공하면 이 답이다(재시작은 요청 직전 측정값).
    Resources(crate::resources::Resources),
    /// `FEAT_RESUME` 필요(MK-052).
    Resumed {
        session: SessionId,
        pid: u32,
    },
    /// `FEAT_GIT_WATCH` 필요(OWE-585).
    GitWatch(crate::git::GitWatchReply),
    Hibernation {
        session: Option<SessionId>,
    },
    WorktreePreparation(WorktreePreparation),
    /// `FEAT_AGENT_HISTORY` 필요(MK-082).
    AgentHistory(crate::agent_history::Response),
    GitEdited(Result<(), String>),
    /// `FEAT_SESSION_ACCOUNT` 필요(OWE-707). [`Request::SessionAccounts`]의 답.
    /// 범위 밖 세션은 담지 않는다. 셸·구 기록 세션은 원소가 없어 배지를 숨긴다.
    SessionAccounts(Vec<SessionAccount>),
    /// `FEAT_CLEAR_SCROLLBACK` 필요(OWE-873b). [`Request::ClearScrollback`]의 답.
    ScrollbackCleared,
    /// `FEAT_CLI` 필요(MK-225). [`Request::SendInput`]의 답.
    InputSent,
    /// 멀티에이전트 오케스트레이션 응답(MK-227).
    Orchestration(crate::orchestration::OrchResponse),
    /// `None` means unchanged, including an empty screen after a previous changed reply.
    CaptureSince {
        stamp: CaptureStamp,
        #[serde(deserialize_with = "optional_capture_bytes")]
        bytes: Option<Vec<u8>>,
    },
    /// `FEAT_REMOTE_FS` 필요. [`Request::RemoteFs`]의 응답.
    RemoteFs(crate::remote_fs::RemoteFsReply),
}

/// Only meaningful in one negotiated daemon incarnation; includes session to prevent cross-scope reuse.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct CaptureStamp {
    pub session: SessionId,
    pub revision: crate::screen::Revision,
    pub seq: crate::screen::Seq,
}

fn optional_capture_bytes<'de, D: serde::Deserializer<'de>>(
    d: D,
) -> Result<Option<Vec<u8>>, D::Error> {
    struct Optional;
    impl<'de> serde::de::Visitor<'de> for Optional {
        type Value = Option<Vec<u8>>;
        fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str("optional bounded plain capture")
        }
        fn visit_none<E: serde::de::Error>(self) -> Result<Self::Value, E> {
            Ok(None)
        }
        fn visit_some<D: serde::Deserializer<'de>>(self, d: D) -> Result<Self::Value, D::Error> {
            capture_bytes(d).map(Some)
        }
    }
    d.deserialize_option(Optional)
}

/// 프로세스가 사라진 세션의 마지막 화면과 스크롤백(디스크 세그먼트 재생, 읽기 전용). 필드는 끝에만 붙인다.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Cold {
    /// 마지막 화면 키프레임([`crate::screen::Op::Reset`]으로 시작).
    pub screen: Vec<crate::screen::Op>,
    /// 화면 위 스크롤백, 오래된 행부터.
    pub history: Vec<Vec<crate::screen::Cell>>,
}

impl Cold {
    /// 프레임 하나에 들어가도록 스크롤백의 오래된 행부터 버린다. 화면만으로 넘치면 `false`.
    pub fn fit(&mut self) -> bool {
        fn size<T: Serialize + ?Sized>(v: &T) -> usize {
            postcard::experimental::serialized_size(v).unwrap_or(usize::MAX)
        }
        let Some(budget) = crate::screen::OPS_BUDGET.checked_sub(size(&self.screen)) else {
            return false;
        };
        let mut used = 0usize;
        let keep = self
            .history
            .iter()
            .rev()
            .take_while(|row| {
                used = used.saturating_add(size(*row));
                used <= budget
            })
            .count();
        let drop = self.history.len().saturating_sub(keep);
        self.history.drain(..drop);
        true
    }
}

impl Msg for Request {
    const KIND: u8 = kind::REQUEST;

    fn encode(&self) -> Result<Vec<u8>, crate::ipc::Error> {
        let (tag, session, create) = match self {
            Self::Create(c) => (0_u8, None, c),
            Self::Resume { session, create } => (16, Some(session), create),
            Self::Hibernation { session, create } => (18, Some(session), create),
            _ => return crate::ipc::encode(Self::KIND, self),
        };
        let mut body = postcard::to_stdvec(&tag).map_err(crate::ipc::Error::Codec)?;
        if let Some(session) = session {
            body.extend(postcard::to_stdvec(session).map_err(crate::ipc::Error::Codec)?);
        }
        body.extend(
            postcard::to_stdvec(&(&create.program, &create.cwd, create.cols, create.rows))
                .map_err(crate::ipc::Error::Codec)?,
        );
        if create.account.is_some() {
            body.extend(postcard::to_stdvec(&create.account).map_err(crate::ipc::Error::Codec)?);
        }
        crate::ipc::encode_frame(Self::KIND, &body)
    }

    fn decode(kind: u8, body: &[u8]) -> Result<Self, crate::ipc::Error> {
        if kind != Self::KIND || !matches!(body.first(), Some(0 | 16 | 18)) {
            return crate::ipc::decode(Self::KIND, kind, body);
        }
        let (tag, rest) =
            postcard::take_from_bytes::<u8>(body).map_err(crate::ipc::Error::Codec)?;
        let (session, rest) = if tag == 0 {
            (None, rest)
        } else {
            let (id, rest) = postcard::take_from_bytes::<crate::SessionId>(rest)
                .map_err(crate::ipc::Error::Codec)?;
            (Some(id), rest)
        };
        let ((program, cwd, cols, rows), rest) =
            postcard::take_from_bytes::<(Program, Vec<u8>, u16, u16)>(rest)
                .map_err(crate::ipc::Error::Codec)?;
        let account = if rest.is_empty() {
            None
        } else {
            postcard::take_from_bytes::<Option<String>>(rest)
                .map_err(crate::ipc::Error::Codec)?
                .0
        };
        if account
            .as_ref()
            .is_some_and(|s| s.is_empty() || s.len() > MAX_AGENT_TEXT)
        {
            return Err(crate::ipc::Error::Codec(
                postcard::Error::DeserializeBadEncoding,
            ));
        }
        let create = Create {
            program,
            cwd,
            cols,
            rows,
            account,
        };
        Ok(match session {
            None => Self::Create(create),
            Some(session) if tag == 16 => Self::Resume { session, create },
            Some(session) => Self::Hibernation { session, create },
        })
    }
}
impl Msg for Reply {
    const KIND: u8 = kind::REPLY;

    fn encode(&self) -> Result<Vec<u8>, crate::ipc::Error> {
        if let Self::States(rows) = self {
            let mut body = postcard::to_stdvec(self).map_err(crate::ipc::Error::Codec)?;
            let has_subagents = rows.iter().any(|r| !r.status.subagents.is_empty());
            let has_title = has_subagents || rows.iter().any(|r| r.status.title.is_some());
            if has_title || rows.iter().any(|r| r.status.foreground.is_some()) {
                for row in rows {
                    body.extend(
                        postcard::to_stdvec(&row.status.foreground)
                            .map_err(crate::ipc::Error::Codec)?,
                    );
                }
            }
            if has_title {
                for row in rows {
                    body.extend(
                        postcard::to_stdvec(&row.status.title).map_err(crate::ipc::Error::Codec)?,
                    );
                }
            }
            if has_subagents {
                for row in rows {
                    body.extend(
                        postcard::to_stdvec(&row.status.subagents)
                            .map_err(crate::ipc::Error::Codec)?,
                    );
                }
            }
            return crate::ipc::encode_frame(Self::KIND, &body);
        }
        if let Self::Sidebar(data) = self {
            let mut body = postcard::to_stdvec(self).map_err(crate::ipc::Error::Codec)?;
            if !data.branches.is_empty() {
                body.extend(postcard::to_stdvec(&data.branches).map_err(crate::ipc::Error::Codec)?);
            }
            return crate::ipc::encode_frame(Self::KIND, &body);
        }
        crate::ipc::encode(Self::KIND, self)
    }

    fn decode(kind: u8, body: &[u8]) -> Result<Self, crate::ipc::Error> {
        if kind == Self::KIND && body.first() == Some(&4) {
            let (mut reply, rest) =
                postcard::take_from_bytes::<Self>(body).map_err(crate::ipc::Error::Codec)?;
            if !rest.is_empty()
                && let Self::States(rows) = &mut reply
            {
                let mut tail = rest;
                for row in rows.iter_mut() {
                    let (fg, next) = postcard::take_from_bytes::<Option<String>>(tail)
                        .map_err(crate::ipc::Error::Codec)?;
                    if fg
                        .as_ref()
                        .is_some_and(|name| name.is_empty() || name.len() > MAX_AGENT_TEXT)
                    {
                        return Err(crate::ipc::Error::Codec(
                            postcard::Error::DeserializeBadEncoding,
                        ));
                    }
                    row.status.foreground = fg;
                    tail = next;
                }
                if !tail.is_empty() {
                    for row in rows.iter_mut() {
                        let (title, next) = postcard::take_from_bytes::<Option<String>>(tail)
                            .map_err(crate::ipc::Error::Codec)?;
                        if title
                            .as_ref()
                            .is_some_and(|t| t.is_empty() || t.len() > MAX_TITLE_TEXT)
                        {
                            return Err(crate::ipc::Error::Codec(
                                postcard::Error::DeserializeBadEncoding,
                            ));
                        }
                        row.status.title = title;
                        tail = next;
                    }
                }
                if !tail.is_empty() {
                    for row in rows.iter_mut() {
                        let (subagents, next) = postcard::take_from_bytes::<Vec<Subagent>>(tail)
                            .map_err(crate::ipc::Error::Codec)?;
                        if subagents.len() > MAX_SUBAGENTS
                            || subagents.iter().any(|a| {
                                a.kind.len() > MAX_AGENT_TEXT
                                    || a.description.len() > MAX_TITLE_TEXT
                            })
                        {
                            return Err(crate::ipc::Error::Codec(
                                postcard::Error::DeserializeBadEncoding,
                            ));
                        }
                        row.status.subagents = subagents;
                        tail = next;
                    }
                }
            }
            return Ok(reply);
        }
        if kind == Self::KIND && body.first() == Some(&14) {
            let (mut reply, rest) =
                postcard::take_from_bytes::<Self>(body).map_err(crate::ipc::Error::Codec)?;
            if !rest.is_empty()
                && let Self::Sidebar(data) = &mut reply
            {
                let (Branches(branches), _) = postcard::take_from_bytes::<Branches>(rest)
                    .map_err(crate::ipc::Error::Codec)?;
                let branches: Vec<_> = branches
                    .into_iter()
                    .map(|Branch(path, branch)| (path, branch))
                    .collect();
                if branches.len() > 512
                    || branches.iter().any(|(path, branch)| {
                        path.is_empty()
                            || path.len() > 4096
                            || branch.is_empty()
                            || branch.len() > MAX_TITLE_TEXT
                    })
                {
                    return Err(crate::ipc::Error::Codec(
                        postcard::Error::DeserializeBadEncoding,
                    ));
                }
                data.branches = branches;
            }
            return Ok(reply);
        }
        crate::ipc::decode(Self::KIND, kind, body)
    }
}

#[cfg(test)]
mod tests;

fn input_bytes<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Vec<u8>, D::Error> {
    bounded::<D, u8, { crate::input::MAX_INPUT }>(d)
}

// Bound the collection before allocating, including forged postcard length hints.
fn capture_bytes<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Vec<u8>, D::Error> {
    struct Bytes;
    impl<'de> serde::de::Visitor<'de> for Bytes {
        type Value = Vec<u8>;
        fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.write_str("bounded CLI capture bytes")
        }
        fn visit_seq<A: serde::de::SeqAccess<'de>>(
            self,
            mut seq: A,
        ) -> Result<Self::Value, A::Error> {
            use serde::de::Error as _;
            if seq.size_hint().is_some_and(|n| n > MAX_CAPTURE) {
                return Err(A::Error::custom("capture exceeds byte limit"));
            }
            let mut bytes = Vec::new();
            while let Some(byte) = seq.next_element()? {
                if bytes.len() >= MAX_CAPTURE {
                    return Err(A::Error::custom("capture exceeds byte limit"));
                }
                bytes.push(byte);
            }
            Ok(bytes)
        }
    }
    d.deserialize_seq(Bytes)
}
