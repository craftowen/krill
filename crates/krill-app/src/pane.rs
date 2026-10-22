//! 칸 하나 ↔ krilld 세션 하나(OWE-490). gpui 없음 — 셸(`main.rs`의 `shell`)이 IO 스레드와 그리기를 맡는다.
//!
//! - **세션**: 칸에 세션이 없거나 krilld 목록에 없으면 제어 연결로 로그인 셸을 만든다([`ensure_session`]). 칸 ↔ 세션은
//!   레이아웃 파일에 저장해 앱을 다시 띄우면 같은 세션에 다시 붙는다(`layout`).
//! - **화면**: 보이는 칸마다 연결 하나로 `Attach(Stream::Screen{known})`([`attach`]) → `Update`를 [`View`]에 적용한다.
//!   적용이 실패하면 RESYNC를 한 번 보내고 스냅샷이 올 때까지 다시 보내지 않는다. 숨긴 칸은 연결을 끊고 상태를 버린다
//!   (MK-035: 앱에 남는 것이 없다).
//! - **입력**: 키 바이트는 칸별 [`Journal`]을 거쳐 같은 연결의 `INPUT`으로 간다(MK-026: 끊긴 동안 보류, 중복 0).
//! - **지연 히스토리**: 위로 스크롤하면 그때 `HISTORY`로 핫 스크롤백을, 다 보면 콜드 페이지를 최신부터 받는다
//!   ([`History`]). 바닥으로 돌아오면 받은 행을 버린다.
//!
//! - **콜드 복원(MK-032)**: 칸의 세션이 krilld 목록에 없으면(재부팅·holder와 krilld 동시 소실) 디스크 기록의 마지막
//!   화면과 스크롤백을 받아 읽기 전용으로 띄운다([`Found::Cold`], [`Pane::show_cold`]). 기록도 없으면 새 세션을 만든다.
//!   잠정 결정 의존: MK-024 세그먼트 저장소(krilld `Request::Cold`)·MK-030 레이아웃. 거부되면 `Found::Cold` 분기와
//!   `show_cold`만 걷어 낸다.
//!
//! 연결은 [`daemon::connect`]로 붙는다(holder까지 죽었을 때만 기동, OWE-491). krilld 기동·재기동은 셸의 감시 연결이 맡는다(MK-025·MK-026).

mod context;
mod hibernation;

use krill_proto::os::net::UnixStream;
use std::io::{self, Write};
use std::net::Shutdown;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use krill_proto::SessionId;
use krill_proto::agent::Permission;
pub use krill_proto::control::Program;
use krill_proto::control::{
    AgentLaunch, AgentSettings, Cold, Create, FEAT_AGENTS, FEAT_CLEAR_SCROLLBACK, FEAT_COLD,
    FEAT_CONTROL, FEAT_RESUME, FEAT_SESSION_FORK, Reply, Request,
};
use krill_proto::daemon::{self, Paths};
use krill_proto::input::{self, InputAck, Journal, Notice, Now};
use krill_proto::ipc::{self, ClientRole};
use krill_proto::roles::{self, FEAT_ROLES};
use krill_proto::screen::{
    ApplyError, Attach, Cell, FEAT_SCREEN, Flags, HistoryRequest, HistoryRows, Known, MAX_COLS,
    MAX_HISTORY_ROWS, MAX_LINES, Msg, Refused, Screen, Source, Stream, Update, View, kind,
};

/// 칸 크기를 아직 재지 못했을 때의 새 세션 크기. 붙은 뒤 [`Pane::fit`]이 칸 크기로 맞춘다.
pub const SIZE: (u16, u16) = (80, 24);
/// 제어 응답·`Attached`를 기다리는 최대 시간.
const REPLY_TIMEOUT: Duration = Duration::from_secs(5);
/// 쓰기가 이만큼 막히면(krilld가 읽지 않음) 연결을 끊고 다시 붙는다.
const WRITE_TIMEOUT: Duration = Duration::from_secs(2);
/// 콜드 페이지와 핫 스크롤백의 겹침을 찾을 때 맞춰 보는 핫 맨 위 행 수.
const OVERLAP_ROWS: usize = 8;

use crate::hosts::HostTarget;

fn hello() -> ipc::ClientHello {
    daemon::client_hello(ClientRole::App, "krill-app")
}

fn connect_error(e: daemon::Error) -> io::Error {
    match e {
        daemon::Error::Io(err) => err,
        other => io::Error::other(other.to_string()),
    }
}

/// 붙은 제어 연결과 협상한 feature 비트.
fn reconnect(target: &HostTarget, need: u64) -> io::Result<(UnixStream, u64)> {
    let c = target.connect(&hello()).map_err(connect_error)?;
    if c.negotiated.features & need != need {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "krilld does not support screen streaming",
        ));
    }
    Ok((c.stream, c.negotiated.features))
}

/// 프레임 하나를 기다린다. 닫혔으면 `UnexpectedEof`.
fn read(s: &mut UnixStream) -> io::Result<(u8, Vec<u8>)> {
    ipc::read_frame(s)?.ok_or_else(|| io::ErrorKind::UnexpectedEof.into())
}

/// [`ensure_session`]의 결과.
#[derive(Debug)]
pub enum Found {
    /// 붙을 세션. 새로 만들었으면 `true`.
    Live(SessionId, bool),
    /// 프로세스가 사라진 세션의 디스크 기록(읽기 전용, MK-032).
    Cold(SessionId, Cold),
}

/// 디스크에 남은 마지막 화면을 읽는다(AS-27: attach 직전 세션이 끝나 `Refused`→`NotFound`가
/// 돼도 빈 화면 대신 마지막 기록을 보여준다). 없거나 실패하면 `None`.
/// 잠정 결정 의존: MK-024 세그먼트 저장소(krilld `Request::Cold`).
pub fn load_cold(target: impl Into<HostTarget>, session: SessionId) -> Option<Cold> {
    let target = target.into();
    let (mut s, features) = reconnect(&target, FEAT_CONTROL).ok()?;
    s.set_read_timeout(Some(REPLY_TIMEOUT)).ok()?;
    if features & FEAT_COLD == 0 {
        return None;
    }
    s.write_all(&Request::Cold { session }.encode().ok()?)
        .ok()?;
    let (k, b) = read(&mut s).ok()?;
    match Reply::decode(k, &b).ok()? {
        Reply::Cold(cold) => Some(cold),
        _ => None,
    }
}

/// 세션 `session`의 데몬 쪽 핫·콜드 스크롤백을 제어 연결로 비운다(OWE-873b).
/// 구버전 krilld가 [`FEAT_CLEAR_SCROLLBACK`]을 광고하지 않으면 요청을 보내지 않고 `Ok(false)`를 돌려준다.
///
/// # Errors
/// krilld 연결·프레임 읽기/쓰기가 실패했거나 데몬이 요청을 거부했다.
pub fn clear_scrollback(target: impl Into<HostTarget>, session: SessionId) -> io::Result<bool> {
    let target = target.into();
    let (mut s, features) = reconnect(&target, FEAT_CONTROL)?;
    if features & FEAT_CLEAR_SCROLLBACK == 0 {
        return Ok(false);
    }
    s.set_read_timeout(Some(REPLY_TIMEOUT))?;
    s.set_write_timeout(Some(WRITE_TIMEOUT))?;
    s.write_all(&Request::ClearScrollback { session }.encode()?)?;
    let (k, b) = read(&mut s)?;
    match Reply::decode(k, &b)? {
        Reply::ScrollbackCleared => Ok(true),
        Reply::Failed(f) => Err(io::Error::other(f.to_string())),
        _ => Err(io::ErrorKind::InvalidData.into()),
    }
}

/// 칸이 붙을 세션. `want`가 krilld 목록에 있으면 그것, 없고 디스크 기록이 남았으면 그 콜드 화면, 둘 다 아니면
/// `program`(앱은 로그인 셸)으로 새 세션([`SIZE`]).
///
/// # Errors
/// krilld에 붙지 못했거나 세션을 만들지 못했다(`Failure` 문구).
pub fn ensure_session(
    target: impl Into<HostTarget>,
    want: Option<SessionId>,
    program: &Program,
) -> io::Result<Found> {
    ensure_session_in(
        target,
        want,
        &Create {
            account: None,
            program: program.clone(),
            cwd: Vec::new(),
            cols: SIZE.0,
            rows: SIZE.1,
        },
    )
}

/// [`ensure_session`]과 같되 새 세션은 지정한 폴더·프로그램으로 만든다. git 저장소 여부는 묻지 않는다.
///
/// # Errors
/// krilld에 붙지 못했거나 세션을 만들지 못했다(`Failure` 문구).
pub fn ensure_session_in(
    target: impl Into<HostTarget>,
    want: Option<SessionId>,
    create: &Create,
) -> io::Result<Found> {
    let target = target.into();
    let (mut s, features) = reconnect(&target, FEAT_CONTROL)?;
    s.set_read_timeout(Some(REPLY_TIMEOUT))?;
    let mut call = |req: &Request| -> io::Result<Reply> {
        s.write_all(&req.encode()?)?;
        let (k, b) = read(&mut s)?;
        Ok(Reply::decode(k, &b)?)
    };
    if let Some(id) = want {
        if let Some(session) = hibernation::wake(&mut call, features, id, create)? {
            return Ok(Found::Live(session, true));
        }
        if let Reply::Sessions(list) = call(&Request::List)?
            && list.iter().any(|i| i.session == id)
        {
            return Ok(Found::Live(id, false));
        }
        if features & krill_proto::usage::FEAT_PROVIDER_DETAILS != 0
            && let Some(replacement) = account_handoff(&target, id)?
        {
            return Ok(Found::Live(replacement, false));
        }
        if features & FEAT_COLD != 0
            && let Reply::Cold(cold) = call(&Request::Cold { session: id })?
        {
            return Ok(Found::Cold(id, cold));
        }
    }
    if create.account.is_some() && features & krill_proto::usage::FEAT_ACCOUNT_POLICY == 0 {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "krilld does not support session account selection",
        ));
    }
    if matches!(&create.program, Program::Agent { id } if id.starts_with("fork:"))
        && features & FEAT_SESSION_FORK == 0
    {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "krilld does not support session forking; update the daemon",
        ));
    }
    let create = Request::Create(create.clone());
    match call(&create)? {
        Reply::Created { session, .. } => Ok(Found::Live(session, true)),
        Reply::Failed(f) => Err(io::Error::other(f.to_string())),
        _ => Err(io::ErrorKind::InvalidData.into()),
    }
}

/// Resolve a durable handoff before a missing old terminal can create a fresh conversation.
fn account_handoff(target: &HostTarget, old: SessionId) -> io::Result<Option<SessionId>> {
    use krill_proto::usage::{UsageReply, UsageRequest};
    let mut connection = target.connect(&hello()).map_err(connect_error)?;
    connection.stream.set_read_timeout(Some(REPLY_TIMEOUT))?;
    connection.stream.set_write_timeout(Some(WRITE_TIMEOUT))?;
    connection
        .stream
        .write_all(&UsageRequest::ListAccounts.encode()?)?;
    let (kind, body) = read(&mut connection.stream)?;
    let UsageReply::Accounts(snapshot) = UsageReply::decode(kind, &body)? else {
        return Err(io::ErrorKind::InvalidData.into());
    };
    let mut current = old;
    for _ in 0..snapshot.details.handoffs.len().min(128) {
        let Some(next) = snapshot
            .details
            .handoffs
            .iter()
            .find(|h| h.from == current)
            .and_then(|h| h.to)
        else {
            break;
        };
        if next == current {
            break;
        }
        current = next;
    }
    Ok((current != old).then_some(current))
}

/// 종료·콜드 세션 `session`을 다시 시작한다(MK-052). krilld가 [`FEAT_RESUME`]을 협상했으면
/// [`Request::Resume`]`(claude --resume <id>, codex resume [<id>])`으로, 구 데몬이면 [`Request::Create`]로
/// 새 세션을 만든다.
///
/// 잠정 결정 의존: OWE-397 [MK-032] 콜드 복원 뷰(`Found::Cold`·`Shell::restart`). 거부되면 이 함수와
/// `Request::Resume` 경로를 걷어 낸다.
///
/// # Errors
/// krilld에 붙지 못했거나 세션을 만들지 못했다(`Failure` 문구).
pub fn resume_session(
    target: impl Into<HostTarget>,
    session: Option<SessionId>,
    program: &Program,
) -> io::Result<SessionId> {
    resume_session_in(
        target,
        session,
        &Create {
            account: None,
            program: program.clone(),
            cwd: Vec::new(),
            cols: SIZE.0,
            rows: SIZE.1,
        },
    )
}

/// [`resume_session`]과 같되 에이전트 기록이 없을 때의 기본 폴더·프로그램을 지정한다.
///
/// # Errors
/// krilld에 붙지 못했거나 세션을 만들지 못했다(`Failure` 문구).
pub fn resume_session_in(
    target: impl Into<HostTarget>,
    session: Option<SessionId>,
    create: &Create,
) -> io::Result<SessionId> {
    let target = target.into();
    let (mut s, features) = reconnect(&target, FEAT_CONTROL)?;
    s.set_read_timeout(Some(REPLY_TIMEOUT))?;
    if create.account.is_some() && features & krill_proto::usage::FEAT_ACCOUNT_POLICY == 0 {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "krilld does not support session account selection",
        ));
    }
    let req = match session.filter(|_| features & FEAT_RESUME != 0) {
        Some(session) => Request::Resume {
            session,
            create: create.clone(),
        },
        None => Request::Create(create.clone()),
    };
    s.write_all(&req.encode()?)?;
    let (k, b) = read(&mut s)?;
    match Reply::decode(k, &b)? {
        Reply::Created { session, .. } | Reply::Resumed { session, .. } => Ok(session),
        Reply::Failed(krill_proto::control::Failure::NoHolder) => Err(io::Error::other(
            krill_proto::control::Failure::NoHolder.to_string(),
        )),
        Reply::Failed(krill_proto::control::Failure::Os(errno)) => {
            Err(io::Error::from_raw_os_error(errno))
        }
        Reply::Failed(f) => Err(io::Error::new(io::ErrorKind::InvalidInput, f.to_string())),
        _ => Err(io::ErrorKind::InvalidData.into()),
    }
}

/// 설정 UI의 IPC 경계. gpui 테스트는 소켓 대신 인메모리 구현을 쓴다(CONVENTIONS §6).
pub trait AgentClient: std::fmt::Debug + Send + Sync {
    fn settings(&self, paths: &Paths, permission: Option<Permission>) -> io::Result<AgentSettings>;

    fn settings_for(
        &self,
        target: &HostTarget,
        permission: Option<Permission>,
    ) -> io::Result<AgentSettings> {
        let fallback = Paths {
            socket: target.socket().to_path_buf(),
            holder: std::path::PathBuf::new(),
            krilld: std::path::PathBuf::new(),
        };
        self.settings(target.local_paths().unwrap_or(&fallback), permission)
    }
}

#[derive(Debug)]
pub struct DaemonAgents;

impl AgentClient for DaemonAgents {
    fn settings(&self, paths: &Paths, permission: Option<Permission>) -> io::Result<AgentSettings> {
        agent_settings(paths, permission)
    }

    fn settings_for(
        &self,
        target: &HostTarget,
        permission: Option<Permission>,
    ) -> io::Result<AgentSettings> {
        agent_settings(target, permission)
    }
}

/// 전역 권한·프로필 조회 또는 저장. 저장 응답을 받은 뒤에만 UI 토글 값을 바꾼다.
pub fn agent_settings(
    target: impl Into<HostTarget>,
    permission: Option<Permission>,
) -> io::Result<AgentSettings> {
    let target = target.into();
    match agent_call(&target, &Request::AgentSettings { permission })? {
        Some(Reply::AgentSettings(settings)) => Ok(settings),
        None => Err(io::ErrorKind::Unsupported.into()),
        _ => Err(io::ErrorKind::InvalidData.into()),
    }
}

fn agent_call(target: &HostTarget, request: &Request) -> io::Result<Option<Reply>> {
    let conn = target.connect(&hello()).map_err(connect_error)?;
    if conn.negotiated.features & (FEAT_CONTROL | FEAT_AGENTS) != FEAT_CONTROL | FEAT_AGENTS {
        return Ok(None); // 이전 데몬에는 새 variant를 보내지 않는다.
    }
    let mut stream = conn.stream;
    stream.set_read_timeout(Some(REPLY_TIMEOUT))?;
    stream.set_write_timeout(Some(WRITE_TIMEOUT))?;
    stream.write_all(&request.encode()?)?;
    let (kind, body) = read(&mut stream)?;
    match Reply::decode(kind, &body)? {
        Reply::Failed(failure) => Err(io::Error::other(failure.to_string())),
        reply => Ok(Some(reply)),
    }
}

/// 화면 연결의 쓰기 쪽. `write` 한 번(프레임 하나)을 쓰기 스레드에 넘기고 바로 돌아온다 — 큰 붙여넣기도 UI 스레드를
/// 막지 않는다(OWE-579). 쓰기가 실패하거나 [`WRITE_TIMEOUT`]만큼 막히면, 또는 이 값을 버리면 스레드가 소켓을 닫는다.
/// 그러면 읽는 쪽이 EOF를 받아 셸이 다시 붙는다(끊긴 채 열린 연결에 입력이 보류되지 않는다).
#[derive(Debug)]
pub struct Writer {
    send: mpsc::SyncSender<PendingWrite>,
    bytes: Arc<AtomicUsize>,
}

/// 설계 상한: ACK 대기 입력 1MiB와 제어 프레임 여유를 합친다. 쓰는 중인 프레임도 포함한다.
const WRITE_QUEUE_BYTES: usize = 2 << 20;
const WRITE_QUEUE_FRAMES: usize = 1_024;

#[derive(Debug)]
struct PendingWrite {
    frame: Vec<u8>,
    bytes: Arc<AtomicUsize>,
}

impl Drop for PendingWrite {
    fn drop(&mut self) {
        self.bytes.fetch_sub(self.frame.len(), Ordering::Relaxed);
    }
}

impl Writer {
    /// # Errors
    /// 소켓 설정이나 스레드 만들기가 실패했다.
    pub fn spawn(s: UnixStream) -> io::Result<Self> {
        Self::spawn_for(s, None)
    }

    fn spawn_for(mut s: UnixStream, target: Option<HostTarget>) -> io::Result<Self> {
        s.set_write_timeout(Some(WRITE_TIMEOUT))?;
        let (tx, rx) = mpsc::sync_channel::<PendingWrite>(WRITE_QUEUE_FRAMES);
        let bytes = Arc::new(AtomicUsize::new(0));
        thread::Builder::new()
            .name("pane-writer".to_owned())
            .spawn(move || {
                let _connection_owner = target;
                while let Ok(frame) = rx.recv() {
                    if s.write_all(&frame.frame).is_err() {
                        break;
                    }
                }
                s.shutdown(Shutdown::Both).ok();
            })?;
        Ok(Self { send: tx, bytes })
    }
}

impl Write for Writer {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.bytes
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |held| {
                held.checked_add(buf.len())
                    .filter(|n| *n <= WRITE_QUEUE_BYTES)
            })
            .map_err(|_| io::Error::from(io::ErrorKind::WouldBlock))?;
        let pending = PendingWrite {
            frame: buf.to_vec(),
            bytes: Arc::clone(&self.bytes),
        };
        self.send.try_send(pending).map_err(|error| match error {
            mpsc::TrySendError::Full(_) => io::Error::from(io::ErrorKind::WouldBlock),
            mpsc::TrySendError::Disconnected(_) => io::Error::from(io::ErrorKind::BrokenPipe),
        })?;
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// 붙은 화면 스트림. `reader`는 IO 스레드가 막고 읽고, `writer`는 UI가 쓴다(같은 소켓).
#[derive(Debug)]
pub struct Linked {
    pub reader: UnixStream,
    pub writer: Writer,
    /// 붙은 krilld 세대(`ServerHello::instance_id`).
    pub instance: u64,
    /// 전역 토글과 무관한 세션 생성 당시의 권한. 이전 데몬·일반 셸은 None.
    pub launch: Option<AgentLaunch>,
    /// `FEAT_ROLES`를 협상했다(칸 크기를 보낼 수 있다).
    pub features: u64,
    /// `FEAT_SEARCH` 협상 여부(MK-059).
    pub search: bool,
}

/// `session`에 화면 스트림으로 붙는다. `known`은 가진 화면(같은 세대·revision이면 krilld가 스냅샷을 생략).
///
/// # Errors
/// 붙지 못했다. 세션이 없으면 `NotFound`.
pub fn attach(
    target: impl Into<HostTarget>,
    session: SessionId,
    known: Option<Known>,
) -> io::Result<Linked> {
    let target = target.into();
    let launch = match agent_call(&target, &Request::AgentLaunch { session })? {
        Some(Reply::AgentLaunch(launch)) => launch,
        None => None,
        _ => return Err(io::ErrorKind::InvalidData.into()),
    };
    let c = target.connect(&hello()).map_err(connect_error)?;
    if c.negotiated.features & FEAT_SCREEN == 0 {
        return Err(io::ErrorKind::Unsupported.into());
    }
    let search = c.negotiated.features & krill_proto::search::FEAT_SEARCH != 0;
    let mut s = c.stream;
    s.write_all(
        &Attach {
            session,
            stream: Stream::Screen { known },
        }
        .encode()?,
    )?;
    s.set_read_timeout(Some(REPLY_TIMEOUT))?;
    let (k, b) = read(&mut s)?;
    if k == kind::REFUSED {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("{:?}", Refused::decode(k, &b)?),
        ));
    }
    krill_proto::screen::Attached::decode(k, &b)?;
    s.set_read_timeout(None)?;
    let writer = Writer::spawn_for(s.try_clone()?, Some(target))?;
    Ok(Linked {
        reader: s,
        writer,
        instance: c.server.instance_id,
        launch,
        features: c.negotiated.features,
        search,
    })
}

/// [`Pane::frame`]의 결과(셸이 할 일).
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Events {
    /// 보이는 화면이 바뀌었다.
    pub redraw: bool,
    pub bell: bool,
    /// 세션이 끝났다(마지막 화면은 남는다).
    pub closed: bool,
    pub notice: Option<Notice>,
    /// 프로그램이 OSC 52로 쓴 클립보드 값(MK-033). 셸이 시스템 클립보드에 쓴다.
    pub clipboard: Option<String>,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
enum State {
    #[default]
    Live,
    /// 콜드 화면(읽기 전용, 연결 없음).
    Cold,
    /// 세션 프로세스가 끝났다(마지막 화면은 남는다, MK-052).
    Closed,
}

#[derive(Debug, Default, Clone)]
struct SearchState {
    supported: bool,
    req: u32,
    pending: Option<u32>,
    query: String,
    cursor: Option<krill_proto::search::Cursor>,
    result: Option<krill_proto::search::SearchResult>,
    /// 스크롤백(Hot/Cold) 일치의 이웃 행 요청 번호. 늦은 답·스크롤·입력·연결 끊김 때 버린다.
    hist_req: Option<u32>,
    /// 이웃 행이 오면 고정 보기로 둘 일치 자리(출처 안 행 번호·하이라이트 범위).
    hist_target: Option<SearchTarget>,
    /// 스크롤백 일치를 고정 보기로 두어 히스토리를 가렸는지. 스크롤·입력·검색 닫기·화면 일치 때 걷는다.
    scrolled: bool,
    /// 현재 활성 일치 1개(데몬이 준 범위).
    highlight: Option<crate::terminal::Highlight>,
    /// 보이는 화면의 일치 범위 전부(활성 포함 가능, 엷게 칠한다).
    highlights: Vec<crate::terminal::Highlight>,
}

/// 이웃 행(`HISTORY_ROWS`)이 오면 고정 보기로 둘 자리.
#[derive(Debug, Clone, Copy)]
struct SearchTarget {
    row: u32,
    column: u16,
    cols: u16,
}

/// 화면에 그릴 검색 일치 상한(OWE-868). 한 글자 검색어·거대 화면에서도 프레임당 quad를 묶는다.
const MAX_SEARCH_MARKS: usize = 1_000;

/// 한 행에서 리터럴 검색어와 겹치지 않게 일치하는 칸 범위들을 돌려준다.
/// 데몬(`krilld::search`)의 `text_spans`와 같은 셀→글자 대응(SPACER 건너뜀, 넓은 문자 2칸)이다.
fn find_matches(row: &[Cell], query: &str) -> Vec<(u16, u16)> {
    if query.is_empty() {
        return Vec::new();
    }
    let mut text = String::new();
    let mut spans: Vec<(usize, usize, u16, u16)> = Vec::new();
    for (column, cell) in row.iter().enumerate() {
        if cell.flags.contains(Flags::SPACER) {
            continue;
        }
        let Ok(column) = u16::try_from(column) else {
            break;
        };
        let start = text.len();
        text.push(cell.ch);
        text.extend(&cell.zerowidth);
        let end_column = column.saturating_add(if cell.flags.contains(Flags::WIDE) {
            2
        } else {
            1
        });
        spans.push((start, text.len(), column, end_column));
    }
    let mut out = Vec::new();
    for (byte, matched) in text.match_indices(query) {
        let end = byte.saturating_add(matched.len());
        let mut overlaps = spans.iter().filter(|span| span.1 > byte && span.0 < end);
        let Some(first) = overlaps.next() else {
            continue;
        };
        let last = overlaps.next_back().unwrap_or(first);
        out.push((first.2, last.3.saturating_sub(first.2)));
    }
    out
}

impl SearchState {
    fn clear(&mut self) {
        self.pending = None;
        self.query.clear();
        self.cursor = None;
        self.result = None;
        self.hist_req = None;
        self.hist_target = None;
        self.scrolled = false;
        self.highlight = None;
        self.highlights.clear();
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AccountInput {
    Normal,
    Switching,
    Delivering,
}

/// 칸 하나의 연결 상태: 화면 스트림·입력 장부·지연 히스토리. IO는 `out`(붙어 있을 때의 쓰기 쪽)에만 한다.
#[derive(Debug)]
pub struct Pane<W> {
    /// 화면·입력이 가리키는 세션.
    session: Option<SessionId>,
    view: View,
    instance: u64,
    features: u64,
    journal: Journal,
    modes: krill_proto::term_modes::Modes,
    history: History,
    context: Option<context::Copy>,
    search: SearchState,
    out: Option<W>,
    /// RESYNC를 보냈고 스냅샷을 기다린다.
    resync: bool,
    state: State,
    shown: Rc<Screen>,
    /// 칸에 들어가는 (열, 행). 셸이 그린 자리로 잰다([`Pane::fit`]).
    grid: Option<(u16, u16)>,
    /// 이 연결로 보낸 크기.
    sent: Option<(u16, u16)>,
    /// 이 연결이 `FEAT_ROLES`를 협상했다.
    roles: bool,
    account_input: AccountInput,
}

impl<W> Default for Pane<W> {
    fn default() -> Self {
        Self {
            session: None,
            view: View::default(),
            instance: 0,
            features: 0,
            journal: Journal::new(),
            modes: krill_proto::term_modes::Modes::default(),
            history: History::default(),
            context: None,
            search: SearchState::default(),
            out: None,
            resync: false,
            state: State::Live,
            shown: Rc::default(),
            grid: None,
            sent: None,
            roles: false,
            account_input: AccountInput::Normal,
        }
    }
}

impl<W: Write> Pane<W> {
    pub fn session(&self) -> Option<SessionId> {
        self.session
    }

    /// 보낸 입력의 ACK가 깨어 있는 시간으로 PROBE 이상 지연됐다. 셸이 배너로 보인다.
    pub fn stalled(&self, now: Now) -> bool {
        self.journal.stalled(now)
    }

    pub fn is_linked(&self) -> bool {
        self.out.is_some()
    }

    pub fn features(&self) -> u64 {
        self.features
    }

    /// 콜드 화면을 띄우고 있다(읽기 전용, MK-032).
    pub fn is_cold(&self) -> bool {
        self.state == State::Cold
    }

    /// 붙어 있는 세션의 프로세스가 끝났다(Restart 칩 표시, MK-052).
    pub fn is_closed(&self) -> bool {
        self.state == State::Closed
    }

    /// 프로세스가 사라진 세션의 마지막 화면과 스크롤백을 읽기 전용으로 띄운다. 입력은 버리고, 스크롤은 받은
    /// 스크롤백 안에서만 한다.
    ///
    /// # Errors
    /// 화면 키프레임이 잘못됐다(아무것도 바꾸지 않는다).
    pub fn show_cold(&mut self, session: SessionId, cold: Cold) -> Result<(), ApplyError> {
        let mut screen = Screen::default();
        screen.apply(cold.screen)?;
        screen.cursor.visible = false;
        *self = Self {
            session: Some(session),
            view: View {
                screen,
                ..View::default()
            },
            history: History::fixed(cold.history),
            state: State::Cold,
            ..Self::default()
        };
        self.refresh();
        Ok(())
    }

    /// 지금 그릴 화면(스크롤했으면 히스토리를 위에 붙인 것). 커서는 krilld 화면 기준이다.
    pub fn shown(&self) -> Rc<Screen> {
        Rc::clone(&self.shown)
    }

    /// krilld 화면(스크롤과 무관).
    pub fn modes(&self) -> krill_proto::term_modes::Modes {
        self.modes
    }

    pub fn screen(&self) -> &Screen {
        &self.view.screen
    }

    /// 재접속 때 [`attach`]에 줄 값. 이 세션의 화면을 끝까지 받았을 때만.
    pub fn known(&self, session: SessionId) -> Option<Known> {
        (self.session == Some(session))
            .then(|| self.view.known(self.instance))
            .flatten()
    }

    /// 칸에 들어가는 (열, 행). 아직 재지 않았으면 `None`.
    pub fn grid(&self) -> Option<(u16, u16)> {
        self.grid
    }

    /// 칸 크기가 (열, 행)이다. 붙어 있고 보낸 크기와 다르면 `Resize`·`Focus`로 PTY를 맞춘다(MK-050: 이 연결이 크기
    /// 주인이 된다). 끊겨 있으면 다음 [`Pane::link`]에서 보낸다. 쓰기가 실패하면 연결을 끊고 그 알림.
    pub fn fit(&mut self, cols: u16, rows: u16) -> Option<Notice> {
        self.grid = Some((cols.clamp(1, MAX_COLS), rows.clamp(1, MAX_LINES)));
        let (cols, rows) = self.grid.filter(|g| self.sent != Some(*g))?;
        if !self.roles || self.out.is_none() {
            return None;
        }
        self.sent = self.grid;
        let failed = self.send(&roles::Request::Resize { cols, rows }).is_err()
            || self.send(&roles::Request::Focus).is_err();
        failed.then(|| self.unlink()).flatten()
    }

    /// `session`에 붙었다. 다른 세션이면 화면을 버리고, 보류 입력은 같은 세션(처음 붙는 칸이면 그 세션)에만 보낸다
    /// (MK-026). 버린 입력은 알림으로. `roles`면 잰 칸 크기를 보낸다([`Pane::fit`]).
    /// 콜드 세션을 재개해 새 세션으로 이어 붙을 때는 콜드 화면·스크롤백을 새 세션 위쪽 히스토리로 보존한다(OWE-842).
    pub fn link(
        &mut self,
        session: SessionId,
        instance: u64,
        out: W,
        features: u64,
        now: Now,
    ) -> Vec<Notice> {
        let was_cold = self.state == State::Cold;
        // 처음 붙는 칸(아직 세션 없음)에서 친 입력은 그 첫 세션에 간다.
        let same = if self.session.is_none_or(|s| s == session) {
            input::Incarnation::Same
        } else {
            input::Incarnation::Changed
        };
        let prior_from_cold = if same == input::Incarnation::Changed && was_cold {
            Some(self.collect_cold_prior())
        } else {
            None
        };
        if same == input::Incarnation::Changed || self.instance != instance {
            self.view = View::default();
            self.modes = krill_proto::term_modes::Modes::default();
            self.state = State::Live;
        }
        if same == input::Incarnation::Changed {
            self.forget_search_view();
            if let Some(prior) = prior_from_cold {
                self.history = History::with_prior(prior);
            } else {
                self.history.clear_all();
            }
            self.search.clear();
        }
        self.search.supported = true;
        self.search.pending = None;
        self.search.cursor = None;
        self.search.hist_req = None;
        self.search.hist_target = None;
        self.session = Some(session);
        self.instance = instance;
        self.out = Some(out);
        self.resync = false;
        self.features = features;
        self.roles = features & FEAT_ROLES != 0;
        self.sent = None;
        if self.state == State::Cold {
            self.state = State::Live;
        }
        let mut notices = if features & krill_proto::input::FEAT_INPUT == 0 {
            self.journal
                .discard_held(input::Fate::Unsupported)
                .into_iter()
                .collect()
        } else {
            Vec::new()
        };
        let (frames, relink_notices) = if self.account_switching() {
            self.journal.pause();
            (Vec::new(), Vec::new())
        } else {
            self.journal.relink(same, now)
        };
        notices.extend(relink_notices);
        for f in frames {
            if let Some(n) = self.send(&f).err().and_then(|()| self.unlink()) {
                notices.push(n);
            }
        }
        if let Some((cols, rows)) = self.grid {
            notices.extend(self.fit(cols, rows));
        }
        notices
    }

    /// 연결이 끊겼다. ack를 못 받은 입력은 결과를 모른다(알림).
    pub fn unlink(&mut self) -> Option<Notice> {
        self.out = None;
        self.context = None;
        self.history.pending = None;
        self.search.pending = None;
        self.search.cursor = None;
        self.search.hist_req = None;
        self.search.hist_target = None;
        self.journal.unlink()
    }

    /// Keep typing in the bounded journal instead of sending new prompts to the old account.
    pub fn pause_account_input(&mut self) {
        self.account_input = AccountInput::Switching;
        self.journal.pause();
    }

    pub fn account_switching(&self) -> bool {
        self.account_input == AccountInput::Switching
    }

    /// Keep a hidden pane alive until its held prompts have reached the replacement PTY.
    pub fn keep_account_input(&self) -> bool {
        self.account_switching()
            || self.account_input == AccountInput::Delivering && self.journal.has_pending_input()
    }

    /// A cancelled switch may continue on the original account. A failed switch drops the held
    /// bytes with a notice, so prompts intended for the new account never reach the original
    /// one, and the pane goes back to the preserved original session. The daemon keeps failed
    /// handoffs in every snapshot, so a pane that is not switching ignores the failure.
    pub fn resume_account_input(&mut self, failed: bool, now: Now) -> Vec<Notice> {
        if !self.account_switching() {
            return Vec::new();
        }
        let mut notices = Vec::new();
        if failed {
            notices.extend(self.journal.discard_held(input::Fate::AccountSwitchFailed));
        }
        self.account_input = if self.journal.has_pending_input() {
            AccountInput::Delivering
        } else {
            AccountInput::Normal
        };
        if self.out.is_some() {
            let (frames, relink_notices) = self.journal.relink(input::Incarnation::Same, now);
            notices.extend(relink_notices);
            for frame in frames {
                if let Some(notice) = self.send(&frame).err().and_then(|()| self.unlink()) {
                    notices.push(notice);
                }
            }
        }
        notices
    }

    /// An explicit account handoff keeps only unsent input. In-flight bytes remain uncertain
    /// and are never replayed to another PTY.
    pub fn handoff_account(&mut self, session: SessionId) -> Option<Notice> {
        let notice = self.unlink();
        let journal = std::mem::take(&mut self.journal);
        let grid = self.grid;
        let account_input = if journal.has_pending_input() {
            AccountInput::Delivering
        } else {
            AccountInput::Normal
        };
        *self = Self {
            session: Some(session),
            journal,
            grid,
            account_input,
            ..Self::default()
        };
        notice
    }

    /// 사용자 입력. 스크롤 중이면 바닥으로 돌아간다. 보류 한도를 넘으면 알림.
    pub fn input(&mut self, bytes: &[u8], now: Now) -> Option<Notice> {
        if self.state != State::Live && !self.account_switching() {
            return None;
        }
        let had_mark = self.search.highlight.take().is_some();
        self.search.hist_req = None;
        self.search.hist_target = None;
        let view_dropped = self.forget_search_view();
        if self.history.offset != 0 || had_mark || view_dropped {
            self.history.reset();
            self.refresh();
        }
        if self.out.is_some() && self.features & krill_proto::input::FEAT_INPUT == 0 {
            return Some(Notice {
                fate: input::Fate::Unsupported,
                bytes: bytes.to_vec(),
            });
        }
        match self.journal.input(bytes, now) {
            Ok(frames) => frames
                .iter()
                .find_map(|f| self.send(f).err().and_then(|()| self.unlink())),
            Err(n) => Some(n),
        }
    }

    /// 화면을 `lines`행 위로(음수면 아래로) 넘긴다. 필요하면 히스토리를 요청한다. 화면이 바뀌면 `true`.
    pub fn scroll(&mut self, lines: isize) -> bool {
        let had_mark = self.search.highlight.take().is_some();
        let had_fetch =
            self.search.hist_req.take().is_some() || self.search.hist_target.take().is_some();
        let view_dropped = self.forget_search_view();
        let before = self.history.visible();
        self.history.scroll(lines);
        self.fetch();
        let changed = self.history.visible() != before || had_mark || had_fetch || view_dropped;
        if changed {
            self.refresh();
        }
        changed
    }

    /// 화면 바닥 위로 올린 행 수(0 = 라이브). 셸이 스크롤바·바닥 복귀 배지를 그릴 때 쓴다(OWE-873).
    pub fn scroll_offset(&self) -> usize {
        self.history.visible()
    }

    /// 받아 둔 스크롤백 행 수(화면 바로 위부터). 아직 도착하지 않은 요청분은 세지 않는다(OWE-873).
    pub fn scrollback_len(&self) -> usize {
        self.history.rows.len()
    }

    /// 스크롤백을 버리고 바닥으로 돌아간다(⌘K, OWE-873 / OWE-873b).
    /// 데몬이 [`FEAT_CLEAR_SCROLLBACK`]을 협상했으면 [`Request::ClearScrollback`]을 함께 보내
    /// 핫·콜드 스크롤백을 비우고, 구버전 데몬이면 로컬 히스토리만 비운다.
    pub fn clear_scrollback(&mut self) {
        self.search.hist_req = None;
        self.search.hist_target = None;
        let view_dropped = self.forget_search_view();
        let had_local = self.history.offset != 0
            || !self.history.rows.is_empty()
            || !self.history.prior.is_empty();
        self.history.clear_scrollback();
        if had_local || view_dropped {
            self.refresh();
        }
        if self.state == State::Live
            && self.out.is_some()
            && self.features & FEAT_CLEAR_SCROLLBACK != 0
            && let Some(session) = self.session
            && self.send(&Request::ClearScrollback { session }).is_err()
        {
            self.unlink();
        }
    }

    /// krilld에서 온 프레임 하나. 연결을 끊어야 하는 오류(모르는 kind·디코드 실패·쓰기 실패)면 `Err`.
    ///
    /// # Errors
    /// 상대가 프로토콜을 어겼거나 응답(RESYNC·HISTORY)을 쓰지 못했다.
    pub fn frame(&mut self, k: u8, body: &[u8]) -> io::Result<Events> {
        let mut ev = Events::default();
        match k {
            krill_proto::term_modes::TERM_MODES => {
                self.modes = krill_proto::term_modes::Modes::decode(k, body)?;
            }
            kind::UPDATE => {
                let u = Update::decode(k, body)?;
                let (done, bells) = (u.done, self.view.screen.bells);
                if self.view.apply(u).is_err() {
                    self.view.revision = None;
                    if !self.resync {
                        self.resync = true;
                        self.write(&ipc::encode_frame(kind::RESYNC, &[])?)?;
                    }
                } else if done {
                    self.resync = false;
                    ev.bell = self.view.screen.bells != bells;
                    ev.redraw = true;
                    self.refresh();
                }
            }
            kind::HISTORY_ROWS => {
                let before = self.history.visible();
                let rows = HistoryRows::decode(k, body)?;
                if self.search_hist_rows(&rows) {
                    ev.redraw = true;
                    self.refresh();
                } else {
                    if self.context_rows(&rows) {
                        ev.clipboard = self.advance_context()?;
                    } else {
                        self.history.rows(rows);
                    }
                    self.fetch();
                    ev.redraw = self.history.visible() != before;
                    if ev.redraw {
                        self.refresh();
                    }
                }
            }
            krill_proto::search::kind::SEARCH_RESULT => {
                let r = krill_proto::search::SearchResult::decode(k, body)?;
                if self.search.pending == Some(r.req) {
                    self.search.pending = None;
                    self.search.hist_req = None;
                    self.search.hist_target = None;
                    // 스크롤백 일치는 이웃 행이 올 때 다시 두기 전에는 활성 표시를 비운다.
                    self.search.highlight = None;
                    if let Some(hit) = &r.hit {
                        self.search.cursor = Some(krill_proto::search::Cursor {
                            snapshot: r.snapshot,
                            index: hit.index,
                        });
                        match hit.source {
                            krill_proto::search::Source::Hot => {
                                self.request_search_neighborhood(Source::Hot, hit);
                            }
                            krill_proto::search::Source::Cold { page } => {
                                self.request_search_neighborhood(Source::Cold { page }, hit);
                            }
                            krill_proto::search::Source::Screen => {
                                self.forget_search_view();
                                self.history.reset();
                                self.search.highlight = Some(crate::terminal::Highlight {
                                    line: u16::try_from(hit.row).unwrap_or(u16::MAX),
                                    column: hit.column,
                                    cols: hit.cols,
                                });
                            }
                        }
                    } else {
                        self.search.cursor = None;
                        self.search.highlight = None;
                        self.forget_search_view();
                    }
                    self.search.result = Some(r);
                    ev.redraw = true;
                    self.refresh();
                }
            }
            input::kind::INPUT_ACK => {
                ev.notice = self.journal.ack(&InputAck::decode(k, body)?);
                if self.account_input == AccountInput::Delivering
                    && !self.journal.has_pending_input()
                {
                    self.account_input = AccountInput::Normal;
                }
            }
            kind::CLOSED => {
                krill_proto::screen::Closed::decode(k, body)?;
                self.state = State::Closed;
                ev.notice = self.unlink();
                self.history.fixed = true;
                self.history.phase = Phase::Done;
                self.view.screen.cursor.visible = false;
                self.refresh();
                ev.closed = true;
                ev.redraw = true;
            }
            krill_proto::clipboard::CLIPBOARD => {
                ev.clipboard = Some(krill_proto::clipboard::Clipboard::decode(k, body)?.text);
            }
            krill_proto::control::kind::REPLY => {
                Reply::decode(k, body)?;
            }
            // REFUSED: 이 연결에서 받을 수 없는 요청을 보냈다(구 krilld). ROLE_ACK: [`Pane::fit`]의 응답(거절돼도 PTY
            // 크기가 그대로일 뿐). 둘 다 무시.
            kind::REFUSED | roles::kind::ROLE_ACK => {}
            _ => return Err(io::ErrorKind::InvalidData.into()),
        }
        Ok(ev)
    }

    fn fetch(&mut self) {
        let lines = usize::from(self.view.screen.lines);
        if self.out.is_some()
            && let Some(req) = self.history.next_request(lines)
            && self.send(&req).is_err()
        {
            self.unlink();
        }
    }

    fn send<M: Msg>(&mut self, m: &M) -> Result<(), ()> {
        let f = m.encode().map_err(|_| ())?;
        self.write(&f).map_err(|_| ())
    }

    fn write(&mut self, frame: &[u8]) -> io::Result<()> {
        match &mut self.out {
            Some(o) => o.write_all(frame),
            None => Ok(()),
        }
    }

    fn refresh(&mut self) {
        let s = self.history.compose(&self.view.screen);
        self.shown = Rc::new(s);
        self.refresh_search_marks();
    }

    /// 보이는 화면의 검색 일치 범위를 전부 다시 찾는다(OWE-868). 활성 일치는 그대로 둔다.
    fn refresh_search_marks(&mut self) {
        self.search.highlights.clear();
        if self.search.query.is_empty() || self.search.result.is_none() {
            return;
        }
        for (line, row) in self.shown.rows.iter().enumerate() {
            if self.search.highlights.len() >= MAX_SEARCH_MARKS {
                break;
            }
            let Ok(line) = u16::try_from(line) else {
                break;
            };
            for (column, cols) in find_matches(row, &self.search.query) {
                if self.search.highlights.len() >= MAX_SEARCH_MARKS {
                    break;
                }
                self.search
                    .highlights
                    .push(crate::terminal::Highlight { line, column, cols });
            }
        }
    }

    /// 스크롤백 일치의 이웃 행을 달라고 데몬에 요청한다(OWE-868). 일치 행이 뷰포트 안에 오도록
    /// `HISTORY` 한 번으로 화면 높이만큼 가져오고, 답이 오면 고정 보기로 둔다. 쓰기가 실패하면 연결을 끊는다.
    fn request_search_neighborhood(&mut self, source: Source, hit: &krill_proto::search::Hit) {
        let lines = usize::from(self.view.screen.lines.max(1));
        let above = lines / 2;
        let below = lines.saturating_sub(above).saturating_sub(1);
        let depth = u64::from(hit.total_rows)
            .saturating_sub(u64::from(hit.row))
            .saturating_sub(1);
        let skip = u32::try_from(depth.saturating_sub(below as u64)).unwrap_or(u32::MAX);
        let count =
            u16::try_from(lines.min(usize::from(MAX_HISTORY_ROWS))).unwrap_or(MAX_HISTORY_ROWS);
        self.history.req = self.history.req.wrapping_add(1);
        let req = self.history.req;
        let request = HistoryRequest {
            req,
            source,
            skip,
            count,
        };
        self.search.hist_req = Some(req);
        self.search.hist_target = Some(SearchTarget {
            row: hit.row,
            column: hit.column,
            cols: hit.cols,
        });
        if self.send(&request).is_err() {
            self.unlink();
        }
    }

    /// 스크롤백 이웃 행 답을 고정 보기로 둔다. 우리 요청이 아니면 `false`(호출자가 다른 용도로 쓴다).
    fn search_hist_rows(&mut self, rows: &HistoryRows) -> bool {
        if self.search.hist_req != Some(rows.req) {
            return false;
        }
        self.search.hist_req = None;
        let Some(target) = self.search.hist_target.take() else {
            return false;
        };
        self.search.highlight = None;
        if rows.rows.is_empty() {
            return true;
        }
        // 답 끝(아래에서 `skip`행 위)이 출처 끝에서 몇 번째인지를 세어 일치 행의 깊이를 잰다.
        let newest = rows.total.saturating_sub(rows.skip).saturating_sub(1);
        let depth = usize::try_from(newest.saturating_sub(target.row))
            .unwrap_or(usize::MAX)
            .saturating_add(1);
        let lines = usize::from(self.view.screen.lines.max(1));
        let offset = rows.rows.len().min(depth.saturating_add(lines / 2));
        self.history = History::fixed(rows.rows.clone());
        self.history.offset = offset;
        self.search.scrolled = true;
        if depth >= 1 && depth <= offset {
            self.search.highlight = Some(crate::terminal::Highlight {
                line: u16::try_from(offset - depth).unwrap_or(u16::MAX),
                column: target.column,
                cols: target.cols,
            });
        }
        true
    }

    /// 검색이 둔 스크롤백 고정 보기를 걷고 라이브 히스토리로 되돌린다. 걷었으면 `true`.
    fn forget_search_view(&mut self) -> bool {
        if !self.search.scrolled {
            return false;
        }
        self.search.scrolled = false;
        let req = self.history.req;
        self.history = History::default();
        self.history.req = req;
        true
    }

    /// `FEAT_SEARCH` 협상 여부. `false`면 [`Pane::search`]가 요청을 보내지 않는다.
    pub fn set_searchable(&mut self, supported: bool) {
        self.search.supported = supported;
    }

    pub fn is_searchable(&self) -> bool {
        self.search.supported
    }

    /// 데몬에 검색 요청(`SearchRequest`)을 보낸다. 같은 검색어로 다시 부르면 직전 결과의 커서를 `after`에 실어
    /// 다음/이전 결과로 넘어간다.
    pub fn search(&mut self, query: &str, direction: krill_proto::search::Direction) -> bool {
        if query.is_empty() || query.len() > krill_proto::search::MAX_QUERY {
            self.clear_search();
            return false;
        }
        if !self.search.supported || self.is_cold() || self.out.is_none() {
            return false;
        }
        let after = if self.search.query == query {
            self.search.cursor
        } else {
            self.search.cursor = None;
            self.search.result = None;
            None
        };
        query.clone_into(&mut self.search.query);
        self.search.req = self.search.req.wrapping_add(1);
        let req = self.search.req;
        self.search.pending = Some(req);
        self.search.hist_req = None;
        self.search.hist_target = None;
        let msg = krill_proto::search::SearchRequest {
            req,
            query: self.search.query.clone(),
            direction,
            after,
        };
        if self.send(&msg).is_err() {
            self.unlink();
            return false;
        }
        true
    }

    /// 검색 하이라이트·일치 범위·스크롤백 고정 보기를 지운다. 손으로 스크롤한 위치는 유지한다.
    pub fn clear_search(&mut self) {
        let had_mark = self.search.highlight.is_some() || !self.search.highlights.is_empty();
        let had_fetch = self.search.hist_req.is_some() || self.search.hist_target.is_some();
        let view_dropped = self.forget_search_view();
        self.search.clear();
        if had_mark || had_fetch || view_dropped {
            self.refresh();
        }
    }

    pub fn highlight(&self) -> Option<crate::terminal::Highlight> {
        self.search.highlight
    }

    /// 보이는 화면의 검색 일치 범위 전부(OWE-868).
    pub fn highlights(&self) -> &[crate::terminal::Highlight] {
        &self.search.highlights
    }

    pub fn search_result(&self) -> Option<&krill_proto::search::SearchResult> {
        self.search.result.as_ref()
    }

    fn collect_cold_prior(&mut self) -> Vec<Vec<Cell>> {
        const MAX_PRIOR_ROWS: usize = 1_000;
        let is_blank_row = |row: &[Cell]| row.iter().all(|c| c.ch == ' ' || c.ch == '\0');
        let screen_rows = &self.view.screen.rows;
        let non_blank_len = screen_rows
            .iter()
            .rposition(|r| !is_blank_row(r))
            .map_or(0, |idx| idx + 1);
        let mut prior = Vec::new();
        for row in screen_rows.iter().take(non_blank_len).rev() {
            if prior.len() >= MAX_PRIOR_ROWS {
                break;
            }
            prior.push(row.clone());
        }
        for row in self.history.rows.drain(..) {
            if prior.len() >= MAX_PRIOR_ROWS {
                break;
            }
            prior.push(row);
        }
        for row in self.history.prior.drain(..) {
            if prior.len() >= MAX_PRIOR_ROWS {
                break;
            }
            prior.push(row);
        }
        prior
    }
}

/// 받는 단계: 핫 스크롤백 → 콜드 페이지(최신부터) → 끝.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    Hot { got: u32 },
    Cold { page: u32, got: u32, newest: bool },
    Done,
}

/// 위로 스크롤한 동안의 히스토리(앱 쪽 지연 히스토리, MK-023 결정 4).
// ponytail: 스크롤 중에 새 출력이 오면 받아 둔 행이 krilld 쪽에서 밀려 위치가 어긋난다(바닥에 갔다 오면 새로 받는다).
// 받은 행은 바닥으로 돌아올 때까지 다 든다(핫 1,000행 + 본 콜드 페이지). 콜드를 깊이 보는 일이 잦으면 창 밖 페이지를 버린다.
#[derive(Debug, Clone)]
pub struct History {
    /// 화면 바닥 위로 올린 행 수(0 = 라이브). 받은 행보다 클 수 있다(받는 중).
    offset: usize,
    /// 받은 행, 화면 바로 위부터 위로.
    rows: Vec<Vec<Cell>>,
    /// 콜드 세션을 재개해 새 세션으로 이어 붙었을 때 보존한 이전 화면·스크롤백(최신 행부터, OWE-842).
    prior: Vec<Vec<Cell>>,
    /// `rows` 중 앞쪽 핫 행 수.
    hot: usize,
    phase: Phase,
    /// 받는 중인 콜드 페이지(아래부터).
    page: Vec<Vec<Cell>>,
    req: u32,
    pending: Option<u32>,
    /// 콜드 화면의 스크롤백: 받은 행이 전부라 바닥으로 와도 버리지 않는다.
    fixed: bool,
}

impl Default for History {
    fn default() -> Self {
        Self {
            offset: 0,
            rows: Vec::new(),
            prior: Vec::new(),
            hot: 0,
            phase: Phase::Hot { got: 0 },
            page: Vec::new(),
            req: 0,
            pending: None,
            fixed: false,
        }
    }
}

impl History {
    /// 바닥으로: 받은 행을 버린다(재개된 콜드 히스토리 `prior`는 유지). 늦게 오는 답은 요청 번호가 달라 버려진다.
    fn reset(&mut self) {
        if self.fixed {
            self.offset = 0;
            return;
        }
        let prior = std::mem::take(&mut self.prior);
        *self = Self {
            req: self.req,
            prior,
            ..Self::default()
        };
    }

    /// 스크롤백 지우기(⌘K, OWE-873b): 고정(콜드·닫힘) 행과 재개 `prior`까지 모두 비우고 바닥으로 돌아간다.
    fn clear_scrollback(&mut self) {
        let fixed = self.fixed;
        *self = Self {
            req: self.req,
            phase: if fixed {
                Phase::Done
            } else {
                Phase::Hot { got: 0 }
            },
            fixed,
            ..Self::default()
        };
    }

    /// 다른 라이브 세션으로 전환할 때 이전 히스토리를 모두 지운다.
    fn clear_all(&mut self) {
        *self = Self {
            req: self.req,
            ..Self::default()
        };
    }

    /// 재개 전 콜드 화면·스크롤백(`prior`, 최신 행부터)을 품은 라이브 히스토리(OWE-842).
    fn with_prior(prior: Vec<Vec<Cell>>) -> Self {
        Self {
            prior,
            ..Self::default()
        }
    }

    /// 다 받은 스크롤백(오래된 행부터).
    fn fixed(rows: Vec<Vec<Cell>>) -> Self {
        let rows: Vec<_> = rows.into_iter().rev().collect();
        Self {
            hot: rows.len(),
            rows,
            phase: Phase::Done,
            fixed: true,
            ..Self::default()
        }
    }

    fn total_len(&self) -> usize {
        self.rows.len().saturating_add(self.prior.len())
    }

    /// 지금 보이는 히스토리 행 수.
    fn visible(&self) -> usize {
        self.offset.min(self.total_len())
    }

    fn scroll(&mut self, lines: isize) {
        // 비동기 요청량이 받은 히스토리보다 커도 이동 기준은 실제 보이는 위치다.
        let next = self.visible().saturating_add_signed(lines);
        // 다 받았으면 더 올라가지 않는다. 받는 중이면 원하는 만큼 두고, 행이 오면 그만큼 보인다.
        let cap = if self.phase == Phase::Done {
            self.total_len()
        } else {
            self.visible().saturating_add(lines.unsigned_abs())
        };
        self.offset = next.min(cap);
        if self.offset == 0 {
            self.reset();
        }
    }

    /// 화면 `lines`행 위까지 채우는 데 더 필요한 요청. 기다리는 답이 있거나 다 받았으면 `None`.
    fn next_request(&mut self, lines: usize) -> Option<HistoryRequest> {
        if self.offset == 0
            || self.pending.is_some()
            || self.rows.len() >= self.offset.saturating_add(lines)
        {
            return None;
        }
        let (source, skip) = match self.phase {
            Phase::Hot { got } => (Source::Hot, got),
            Phase::Cold { page, got, .. } => (Source::Cold { page }, got),
            Phase::Done => return None,
        };
        self.req = self.req.wrapping_add(1);
        self.pending = Some(self.req);
        Some(HistoryRequest {
            req: self.req,
            source,
            skip,
            count: MAX_HISTORY_ROWS,
        })
    }

    fn rows(&mut self, r: HistoryRows) {
        if self.pending != Some(r.req) {
            return;
        }
        self.pending = None;
        let n = u32::try_from(r.rows.len()).unwrap_or(u32::MAX);
        let first_cold = |pages: u32| match pages.checked_sub(1) {
            Some(page) => Phase::Cold {
                page,
                got: 0,
                newest: true,
            },
            None => Phase::Done,
        };
        match self.phase {
            Phase::Hot { got } => {
                self.rows.extend(r.rows.into_iter().rev());
                self.hot = self.rows.len();
                let got = got.saturating_add(n);
                self.phase = if n == 0 || got >= r.total {
                    first_cold(r.pages)
                } else {
                    Phase::Hot { got }
                };
            }
            Phase::Cold { page, got, newest } => {
                self.page.extend(r.rows.into_iter().rev());
                let got = got.saturating_add(n);
                if n != 0 && got < r.total {
                    self.phase = Phase::Cold { page, got, newest };
                    return;
                }
                let page_rows = std::mem::take(&mut self.page);
                let start = if newest {
                    after_overlap(&page_rows, self.rows.get(..self.hot).unwrap_or_default())
                } else {
                    0
                };
                self.rows.extend(page_rows.into_iter().skip(start));
                self.phase = match page.checked_sub(1) {
                    Some(page) => Phase::Cold {
                        page,
                        got: 0,
                        newest: false,
                    },
                    None => Phase::Done,
                };
            }
            Phase::Done => {}
        }
    }

    /// 화면 위에 보이는 히스토리 행을 붙인 화면. 스크롤하지 않았으면 그대로.
    fn compose(&self, screen: &Screen) -> Screen {
        let off = self.visible();
        if off == 0 {
            return screen.clone();
        }
        // 보이는 줄 l은 화면 맨 위에서 l - off번째 줄이다. 음수면 히스토리(화면 바로 위가 0번)에서 off - l - 1번째.
        let pick = |l: usize| {
            if let Some(v) = l.checked_sub(off) {
                (
                    screen.rows.get(v).cloned().unwrap_or_default(),
                    screen.links.get(v).cloned().unwrap_or_default(),
                )
            } else {
                let idx = off - l - 1;
                let row = self
                    .rows
                    .get(idx)
                    .or_else(|| self.prior.get(idx.saturating_sub(self.rows.len())))
                    .cloned()
                    .unwrap_or_default();
                (row, Vec::new())
            }
        };
        let mut s = screen.clone();
        (s.rows, s.links) = (0..screen.rows.len()).map(pick).unzip();
        s.cursor.visible = false;
        s
    }
}

/// 가장 최신 콜드 페이지(아래부터 `page`)에서 핫 스크롤백(아래부터 `hot`)보다 오래된 행이 시작하는 자리. 그 페이지는
/// 지금 기록 중인 세그먼트라 아래쪽이 핫·화면과 겹친다. 핫 맨 위 행들(최대 [`OVERLAP_ROWS`])이 이어서 맞는 첫 자리 바로
/// 위부터 남긴다. 못 찾으면 페이지 전체(겹친 행이 두 번 보일 수 있다).
// ponytail: 행 값 비교라 krilld 재기동·리사이즈로 폭이 달라진 행은 못 맞춘다. 정확하게 하려면 행마다 원본 seq를 싣는다.
fn after_overlap(page: &[Vec<Cell>], hot: &[Vec<Cell>]) -> usize {
    let Some(top) = hot.len().checked_sub(1) else {
        return 0;
    };
    let window = hot.len().min(OVERLAP_ROWS);
    (0..page.len())
        .find(|&j| (0..window.min(j + 1)).all(|m| page.get(j - m) == hot.get(top - m)))
        .map_or(0, |j| j + 1)
}

#[cfg(test)]
#[cfg(unix)]
mod tests;
