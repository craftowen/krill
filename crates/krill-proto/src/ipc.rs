//! 앱·`krill`·`krill hook` ↔ krilld IPC v1 (MK-021, T2-D6). 전송로는 UDS 하나(`SOCK_STREAM`), 소켓 이름에 버전 없음.
//!
//! # 프레임
//!
//! ```text
//! [u32 LE len][u8 kind][body]
//! ```
//!
//! - `len` = `kind`·`body`를 합친 길이(1 이상, [`MAX_FRAME`] 이하). 범위를 벗어나면 받는 쪽은 body를 읽지 않고
//!   연결을 끊는다.
//! - 제어 kind의 body는 그 kind 전용 고정 DTO를 postcard로 인코딩한 것이다. 예외는 원본 바이트 [`crate::screen::kind::OUTPUT`](raw).
//!
//! # 핸드셰이크
//!
//! 1. krilld는 accept 직후 피어 UID를 검사한다(`getpeereid`). 다른 UID면 아무것도 보내지 않고 닫는다.
//! 2. krilld가 [`ServerHello`]를 보내고, 클라이언트가 [`ClientHello`]로 답한다.
//! 3. 양쪽이 [`negotiate`]로 같은 결과(겹치는 최대 버전, feature 교집합)를 계산한다. 겹치는 버전이 없으면
//!    krilld는 닫는다. 클라이언트는 `ServerHello`만 보고 미리 알 수 있다.
//!
//! # 호환 규칙 (N-1)
//!
//! - kind 값은 영구다. 지우거나 다른 뜻으로 다시 쓰지 않는다.
//! - DTO 필드는 끝에만 붙인다. postcard는 필드 순서 기반이라 받는 쪽은 아는 필드까지 읽고 남은 바이트를
//!   무시한다(구 클라이언트가 새 `ServerHello`를 읽는다). 새 krilld가 구 `ClientHello`를 읽어야 하면 필드를 늘린
//!   DTO로 실패할 때 이전 DTO로 다시 읽는다. `#[non_exhaustive]`만으로는 호환되지 않는다.
//! - krilld는 [`PROTO_MAX`]`-1` 클라이언트를 받는다([`PROTO_MIN`]).
//! - 새 kind는 협상한 feature 비트가 있을 때만 보낸다. 받는 쪽은 모르는 kind를 오류로 본다.
//! - `ipc/tests.rs`의 고정 바이트 fixture가 바뀌면 호환 위반이다(새 메시지는 fixture를 추가만 한다).

use std::fmt;
use std::io::{self, Read};

use serde::{Deserialize, Serialize};

/// 이 빌드가 말하는 최고 프로토콜 버전.
pub const PROTO_MAX: u16 = 1;
/// 이 빌드가 받는 최저 프로토콜 버전. `PROTO_MAX`를 올릴 때 `PROTO_MAX - 1`보다 크면 안 된다(N-1 호환).
pub const PROTO_MIN: u16 = 1;
/// Git 원격 동기화 동작(`Edit::Push`·`Pull`·`Sync`·`CommitAndPush`, MK-184·OWE-738 지휘자 배정).
/// [`crate::control::FEAT_CONTROL`]·[`crate::control::FEAT_GIT_EDIT`]와 함께 협상한다.
pub const FEAT_GIT_SYNC: u64 = 1 << 41;
/// 이 빌드가 지원하는 feature 비트.
pub const FEATURES: u64 = crate::screen::FEAT_HISTORY_VIEW
    | crate::screen::FEAT_SCREEN
    | crate::control::FEAT_CONTROL
    | crate::input::FEAT_INPUT
    | crate::hook::FEAT_HOOK
    | crate::control::FEAT_WORKTREE
    | crate::control::FEAT_WORKTREE_REMOVE
    | crate::daemon::FEAT_HOLDER_WATCH
    | crate::daemon::FEAT_HEARTBEAT
    | crate::control::FEAT_GIT_CHANGES
    | crate::screen::FEAT_SCREEN_META
    | crate::control::FEAT_CLI
    | crate::search::FEAT_SEARCH
    | crate::search::FEAT_SEARCH_OPTIONS
    | crate::control::FEAT_CAPTURE_SINCE
    | crate::control::FEAT_WAIT_STATUS
    | crate::term_modes::FEAT_TERM_MODES
    | crate::term_modes::FEAT_CLEAR_VIEWPORT
    | crate::history::FEAT_HISTORY
    | crate::roles::FEAT_ROLES
    | crate::control::FEAT_SIDEBAR
    | crate::control::FEAT_AGENTS
    | crate::control::FEAT_UPGRADE
    | crate::control::FEAT_WINDOWS_UPGRADE
    | crate::control::FEAT_COLD
    | crate::resources::FEAT_RESOURCES
    | crate::clipboard::FEAT_CLIPBOARD
    | FEAT_GIT_SYNC
    | crate::control::FEAT_RESUME
    | crate::control::FEAT_RESUME_HISTORY
    | crate::theme::FEAT_THEME
    | crate::control::FEAT_GIT_WATCH
    | crate::control::FEAT_HIBERNATION
    | crate::control::FEAT_WORKTREE_PREPARE
    | crate::agent_history::FEAT_AGENT_HISTORY
    | crate::agent_history::FEAT_AI_VAULT
    | crate::agent_history::FEAT_CHAT_POLL
    | crate::control::FEAT_GIT_EDIT
    | crate::usage::FEAT_USAGE
    | crate::usage::FEAT_USAGE_AGY
    | crate::usage::FEAT_USAGE_RULES
    | crate::usage::FEAT_USAGE_PROVIDER_METRICS
    | crate::usage::FEAT_USAGE_STATUS
    | crate::usage::FEAT_USAGE_RATELIMIT
    | crate::usage::FEAT_USAGE_MANAGE
    | crate::usage::FEAT_ACCOUNT_CONNECTION
    | crate::usage::FEAT_USAGE_RESET
    | crate::usage::FEAT_USAGE_REFRESH
    | crate::usage::FEAT_PROVIDER_DETAILS
    | crate::usage::FEAT_ACCOUNT_CANCEL
    | crate::usage::FEAT_ACCOUNT_POLICY
    | crate::history::FEAT_HISTORY_PAUSE
    | crate::control::FEAT_SESSION_ACCOUNT
    | crate::control::FEAT_SESSION_FORK
    | crate::control::FEAT_CLEAR_SCROLLBACK
    | crate::control::FEAT_REMOTE_FS
    | crate::mobile::FEAT_MOBILE
    | crate::orchestration::FEAT_ORCH_RETAIN;
// 기능 비트는 서로 겹치면 안 된다(2026-10-15 번호 충돌 사고 방지). 비트가 모두 다르면 합의 1 비트 수가 항목 수와 같다.
const _: () = assert!(FEATURES.count_ones() == 58);
/// 프레임 최대 길이(`len` 값 기준).
pub const MAX_FRAME: usize = 1 << 20;
/// [`ClientHello::name`] 최대 바이트.
pub const NAME_MAX: usize = 256;

/// 제어 kind. 값은 영구다.
pub mod kind {
    pub const SERVER_HELLO: u8 = 0x01;
    pub const CLIENT_HELLO: u8 = 0x02;
}

/// krilld가 accept 직후 보내는 첫 프레임([`kind::SERVER_HELLO`]).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServerHello {
    pub proto_min: u16,
    pub proto_max: u16,
    pub features: u64,
    /// krilld 빌드 문자열(진단용).
    pub build: String,
    /// krilld 세대 식별자. 재기동하면 바뀐다.
    pub instance_id: u64,
}

/// 클라이언트의 답([`kind::CLIENT_HELLO`]).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClientHello {
    pub proto_min: u16,
    pub proto_max: u16,
    pub features: u64,
    pub role: ClientRole,
    /// 클라이언트 이름(진단용, [`NAME_MAX`] 바이트 이하).
    pub name: String,
}

/// 연결한 쪽의 종류. variant는 끝에만 붙인다.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ClientRole {
    App,
    Cli,
    Hook,
}

/// 협상 결과.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Negotiated {
    pub proto: u16,
    pub features: u64,
}

/// 겹치는 최대 버전과 feature 교집합. 겹치는 버전이 없으면 `None`.
pub fn negotiate(server: &ServerHello, client: &ClientHello) -> Option<Negotiated> {
    let lo = server.proto_min.max(client.proto_min);
    let hi = server.proto_max.min(client.proto_max);
    (lo <= hi).then_some(Negotiated {
        proto: hi,
        features: server.features & client.features,
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// `len`이 0이거나 [`MAX_FRAME`]을 넘는다.
    BadLength(u64),
    /// 기다리던 kind가 아니다.
    UnexpectedKind { want: u8, got: u8 },
    /// postcard 인코딩·디코딩 실패(body가 DTO로 읽히지 않음 등).
    Codec(postcard::Error),
    /// 문자열이 상한을 넘는다.
    TooLong(usize),
    /// 겹치는 프로토콜 버전이 없다.
    Incompatible,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::BadLength(n) => write!(f, "bad frame length {n}"),
            Self::UnexpectedKind { want, got } => {
                write!(f, "expected kind {want:#04x}, got {got:#04x}")
            }
            Self::Codec(e) => write!(f, "bad body: {e}"),
            Self::TooLong(n) => write!(f, "string too long ({n} bytes)"),
            Self::Incompatible => f.write_str("no common protocol version"),
        }
    }
}

impl std::error::Error for Error {}

impl From<Error> for io::Error {
    fn from(e: Error) -> Self {
        io::Error::new(io::ErrorKind::InvalidData, e)
    }
}

impl ServerHello {
    /// 프레임 전체를 만든다.
    pub fn encode(&self) -> Result<Vec<u8>, Error> {
        encode(kind::SERVER_HELLO, self)
    }

    pub fn decode(kind: u8, body: &[u8]) -> Result<Self, Error> {
        decode(kind::SERVER_HELLO, kind, body)
    }
}

impl ClientHello {
    /// 프레임 전체를 만든다.
    pub fn encode(&self) -> Result<Vec<u8>, Error> {
        encode(kind::CLIENT_HELLO, self)
    }

    pub fn decode(kind: u8, body: &[u8]) -> Result<Self, Error> {
        let h: Self = decode(kind::CLIENT_HELLO, kind, body)?;
        if h.name.len() > NAME_MAX {
            return Err(Error::TooLong(h.name.len()));
        }
        Ok(h)
    }
}

pub(crate) fn encode<T: Serialize>(kind: u8, v: &T) -> Result<Vec<u8>, Error> {
    let body = postcard::to_stdvec(v).map_err(Error::Codec)?;
    encode_frame(kind, &body)
}

pub(crate) fn decode<'a, T: Deserialize<'a>>(
    want: u8,
    got: u8,
    body: &'a [u8],
) -> Result<T, Error> {
    if want != got {
        return Err(Error::UnexpectedKind { want, got });
    }
    postcard::from_bytes(body).map_err(Error::Codec)
}

/// `kind`와 `body`로 프레임 전체를 만든다. [`MAX_FRAME`]을 넘으면 `BadLength`.
pub fn encode_frame(kind: u8, body: &[u8]) -> Result<Vec<u8>, Error> {
    let n = body.len().checked_add(1).filter(|n| *n <= MAX_FRAME);
    let len = n
        .and_then(|n| u32::try_from(n).ok())
        .ok_or(Error::BadLength(
            u64::try_from(body.len()).unwrap_or(u64::MAX),
        ))?;
    let mut out = Vec::with_capacity(body.len().saturating_add(5));
    out.extend_from_slice(&len.to_le_bytes());
    out.push(kind);
    out.extend_from_slice(body);
    Ok(out)
}

/// 프레임 하나를 막고 읽는다. 프레임 경계에서 상대가 닫았으면 `Ok(None)`.
/// `len`이 범위를 벗어나면 body를 읽거나 할당하지 않고 `InvalidData`.
pub fn read_frame(r: &mut impl Read) -> io::Result<Option<(u8, Vec<u8>)>> {
    let mut head = [0u8; 4];
    match r.read_exact(&mut head) {
        Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
        res => res?,
    }
    let len = u32::from_le_bytes(head);
    let body_len = usize::try_from(len)
        .ok()
        .filter(|n| (1..=MAX_FRAME).contains(n))
        .and_then(|n| n.checked_sub(1))
        .ok_or(Error::BadLength(u64::from(len)))?;
    let mut kind = [0u8; 1];
    r.read_exact(&mut kind)?;
    let mut body = vec![0; body_len];
    r.read_exact(&mut body)?;
    let [kind] = kind;
    Ok(Some((kind, body)))
}

#[cfg(test)]
mod tests;
