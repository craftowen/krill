//! holder ↔ krilld 내부 프로토콜 v1 (MK-018, T2-D9). **동결**: 새 kind 추가와 body 끝 필드 추가만 허용한다.
//!
//! 전송로는 holder가 krilld를 띄울 때 만든 `AF_UNIX` `SOCK_STREAM` socketpair다(krilld 쪽은 krilld의 stdin).
//!
//! # 프레임
//!
//! ```text
//! [u32 LE len][u8 kind][u8 fds][body]
//! ```
//!
//! - `len` = `kind`·`fds`·`body`를 합친 길이(2 이상, [`MAX_FRAME`] 이하).
//! - `fds` = 이 프레임에 `SCM_RIGHTS`로 붙은 fd 수. fd는 프레임 첫 바이트를 보내는 `sendmsg`에 함께 싣는다.
//!   받는 쪽은 도착 순서대로 fd를 쌓고 프레임이 완성되면 앞에서 `fds`개를 꺼낸다.
//! - body: 정수는 LE 고정 길이, 바이트열은 `[u32 LE 길이][바이트]`, 목록은 `[u32 LE 개수][항목…]`,
//!   `Option<i32>`는 `[u8 0]` 또는 `[u8 1][i32 LE]`.
//!
//! # 호환 규칙
//!
//! - 받는 쪽은 body 끝에 남은 바이트를 무시한다. 새 필드는 끝에만 붙인다.
//! - 모르는 kind는 붙은 fd를 닫고 버린다. krilld→holder 요청은 모두 body가 `req: u32`로 시작하므로
//!   holder는 모르는 요청에 [`FromHolder::Failed`]`{req, errno: ENOSYS}`로 답한다([`request_id`]).
//!
//! # 흐름
//!
//! - krilld가 (재)기동되면 holder가 먼저 `Hello` → 보관 중인 세션마다 `Session`(+master fd) →
//!   보관 중인 listen 소켓이 있으면 `Listener`(+fd) → `Ready`를 보낸다.
//! - 요청(`Spawn`·`Kill`·`Release`·`HoldListener`·`UpgradeKrilld`·`UpgradeHolder`·`Adopt`·`Terminate`)에는
//!   `Spawned`/`Done`/`Failed` 중 하나가 같은 `req`로 온다. `Exited`는 아무 때나 온다.
//! - `UpgradeHolder`를 보낸 krilld는 답이 올 때까지 아무것도 보내지 않는다. 성공하면 새 holder 이미지가
//!   `Hello`(새 버전) → `Done{req}`를 보낸다(세션 목록은 다시 보내지 않는다).
//!
//! # v2 추가 (OWE-478, holder만 크래시한 뒤 재결합 — docs/spikes/MK-018.md '재결합 절차')
//!
//! - 요청 `Adopt`(kind `0x07`, +fd): 다른 holder가 띄운 세션(master fd)을 맡긴다. holder의 자식이 아니라
//!   `waitpid`로 거둘 수 없으므로 holder는 `EVFILT_PROC NOTE_EXIT`로 종료만 감지하고, 그 세션의
//!   `Exited`·`Session`의 status는 [`STATUS_UNKNOWN`]이다.
//! - v1 holder는 `Adopt`에 `Failed{req, ENOSYS}`로 답한다(위 호환 규칙). krilld는 `Hello.version`이 2 이상일
//!   때만 `Adopt`를 보낸다. [`STATUS_UNKNOWN`]은 `Adopt`한 세션에만 오므로 v1 krilld는 받을 일이 없다.
//!
//! # v3 추가 (OWE-416, MK-051 — T2 §3.5 종료 순서)
//!
//! - 요청 `Terminate`(kind `0x08`): 세션 프로세스 그룹과 `setsid`로 빠져나간 후손까지 단계적으로 끝낸다.
//!   holder는 첫 단계(SIGHUP)를 보낸 뒤 `Done`으로 답하고, 나머지 단계(2초 뒤 SIGTERM, 다시 3초 뒤 SIGKILL)는
//!   스스로 진행한다. 세션 종료는 평소처럼 `Exited`로 온다. 끝난·없는 세션은 `ESRCH`.
//!   v2 이하 holder는 `ENOSYS`로 답하므로 krilld는 `Hello.version`이 3 이상일 때만 보낸다.
//! - 세션 상한: holder가 보관하는 세션(끝났지만 `Release` 전인 것 포함)이 [`MAX_SESSIONS`]개면
//!   `Spawn`은 `Failed{errno: `[`SESSION_LIMIT_ERRNO`]`}`. `Adopt`는 상한을 보지 않는다(재결합 중 세션을 잃지 않게).

use std::fmt;

use crate::SessionId;

/// 프로토콜 버전. 동결 규칙 안에서 기능을 더하면 올린다.
pub const VERSION: u16 = 3;
/// "종료 코드 없음" 표시값(v2). `waitpid` 원시 상태로 -1은 하위 7비트가 `0x7f`(`WIFSTOPPED`)라
/// 종료 상태(`WIFEXITED`·`WIFSIGNALED`)와 겹치지 않는다.
pub const STATUS_UNKNOWN: i32 = -1;
/// (v3) holder가 보관하는 세션 수 상한. macOS 전체 PTY 한도 `kern.tty.ptmx_max`(511)의 절반(T2 §3.5).
pub const MAX_SESSIONS: usize = 256;
/// (v3) 세션 상한에 걸린 `Spawn`의 errno: macOS `EPROCLIM`(67, "Too many processes").
/// `Spawn`의 다른 실패(openpty·fork·exec)는 이 값을 내지 않아 krilld가 상한 초과로 구별할 수 있다.
pub const SESSION_LIMIT_ERRNO: i32 = 67;
/// 프레임 최대 길이(`len` 값 기준).
pub const MAX_FRAME: usize = 1 << 20;
/// `len` 필드(4바이트) 뒤 `kind`·`fds`(2바이트).
const HEADER: usize = 6;

/// PTY 크기(`TIOCSWINSZ`의 열·행).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WinSize {
    pub cols: u16,
    pub rows: u16,
}

/// krilld → holder 요청. 모두 `req`로 시작한다.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToHolder {
    /// 새 PTY를 열고 `argv`를 자식으로 띄운다(setsid, 제어 터미널 = 새 PTY). `env`가 자식 환경 전부다.
    Spawn {
        req: u32,
        id: SessionId,
        argv: Vec<Vec<u8>>,
        env: Vec<(Vec<u8>, Vec<u8>)>,
        cwd: Vec<u8>,
        size: WinSize,
    },
    /// 세션 프로세스 그룹에 시그널을 보낸다(`killpg`).
    Kill {
        req: u32,
        id: SessionId,
        signal: i32,
    },
    /// holder가 보관하던 master fd와 종료 상태를 버린다.
    Release { req: u32, id: SessionId },
    /// fd 1개(listen 소켓)를 맡긴다. krilld가 다시 뜨면 `Listener`로 돌려준다. 이전 것은 닫는다.
    HoldListener { req: u32 },
    /// 다음 krilld 기동부터 `path`를 실행한다. 이 요청 뒤 krilld가 끝나면 백오프 없이 바로 띄운다.
    UpgradeKrilld { req: u32, path: Vec<u8> },
    /// holder가 `path`로 exec 핸드오프한다(PID·자식·fd 유지).
    UpgradeHolder { req: u32, path: Vec<u8> },
    /// (v2) 이미 도는 세션 `pid`와 그 master fd(1개)를 맡긴다. 종료 코드는 받을 수 없다([`STATUS_UNKNOWN`]).
    Adopt { req: u32, id: SessionId, pid: u32 },
    /// (v3) 세션 프로세스 그룹과 빠져나간 후손을 SIGHUP → SIGTERM → SIGKILL 단계로 끝낸다.
    Terminate { req: u32, id: SessionId },
}

/// holder → krilld 메시지.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FromHolder {
    Hello {
        version: u16,
        holder_pid: u32,
    },
    /// 보관 중인 세션 하나(+master fd). `status`는 이미 끝났으면 `waitpid` 원시 상태 또는 [`STATUS_UNKNOWN`].
    Session {
        id: SessionId,
        pid: u32,
        status: Option<i32>,
    },
    /// 맡겨 둔 listen 소켓(+fd).
    Listener,
    /// 복원 목록 끝.
    Ready,
    /// `Spawn` 성공(+master fd).
    Spawned {
        req: u32,
        id: SessionId,
        pid: u32,
    },
    /// 세션 프로세스가 끝났다. `status`는 `waitpid` 원시 상태 또는 [`STATUS_UNKNOWN`].
    Exited {
        id: SessionId,
        status: i32,
    },
    Done {
        req: u32,
    },
    /// 요청 실패. `errno`는 macOS errno 값.
    Failed {
        req: u32,
        errno: i32,
    },
}

mod kind {
    pub const SPAWN: u8 = 0x01;
    pub const KILL: u8 = 0x02;
    pub const RELEASE: u8 = 0x03;
    pub const HOLD_LISTENER: u8 = 0x04;
    pub const UPGRADE_KRILLD: u8 = 0x05;
    pub const UPGRADE_HOLDER: u8 = 0x06;
    pub const ADOPT: u8 = 0x07;
    pub const TERMINATE: u8 = 0x08;
    pub const HELLO: u8 = 0x81;
    pub const SESSION: u8 = 0x82;
    pub const LISTENER: u8 = 0x83;
    pub const READY: u8 = 0x84;
    pub const SPAWNED: u8 = 0x85;
    pub const EXITED: u8 = 0x86;
    pub const DONE: u8 = 0x87;
    pub const FAILED: u8 = 0x88;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// `len`이 2 미만이거나 [`MAX_FRAME`]을 넘는다.
    BadLength(u32),
    /// body가 필드보다 짧다.
    Truncated,
    /// 이 버전이 모르는 kind.
    UnknownKind(u8),
    /// 프레임의 `fds`가 kind가 요구하는 수와 다르다.
    FdCount { kind: u8, got: u8 },
    /// 잘못된 `Option` 태그.
    BadTag(u8),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::BadLength(n) => write!(f, "bad frame length {n}"),
            Self::Truncated => f.write_str("truncated body"),
            Self::UnknownKind(k) => write!(f, "unknown kind {k:#04x}"),
            Self::FdCount { kind, got } => write!(f, "kind {kind:#04x} carried {got} fds"),
            Self::BadTag(t) => write!(f, "bad option tag {t}"),
        }
    }
}

impl std::error::Error for Error {}

/// 버퍼 앞의 완성된 프레임 하나.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Frame<'a> {
    pub kind: u8,
    pub fds: u8,
    pub body: &'a [u8],
}

/// `buf` 앞에서 완성된 프레임 하나와 그 전체 길이를 꺼낸다. 아직 덜 왔으면 `Ok(None)`.
pub fn parse_frame(buf: &[u8]) -> Result<Option<(Frame<'_>, usize)>, Error> {
    let Some((len, rest)) = buf.split_first_chunk::<4>() else {
        return Ok(None);
    };
    let len = u32::from_le_bytes(*len);
    let n = usize::try_from(len).map_err(|_| Error::BadLength(len))?;
    if !(2..=MAX_FRAME).contains(&n) {
        return Err(Error::BadLength(len));
    }
    let Some((frame, _)) = rest.split_at_checked(n) else {
        return Ok(None);
    };
    let Some((&[kind, fds], body)) = frame.split_first_chunk::<2>() else {
        return Err(Error::BadLength(len));
    };
    let total = n.checked_add(4).ok_or(Error::BadLength(len))?;
    Ok(Some((Frame { kind, fds, body }, total)))
}

/// 요청 body 앞의 `req`. 모르는 kind에 `Failed`로 답할 때 쓴다.
pub fn request_id(body: &[u8]) -> Option<u32> {
    body.first_chunk::<4>().map(|b| u32::from_le_bytes(*b))
}

impl ToHolder {
    /// 이 메시지에 붙는 fd 수.
    pub fn fds(&self) -> u8 {
        u8::from(matches!(
            self,
            Self::HoldListener { .. } | Self::Adopt { .. }
        ))
    }

    pub fn req(&self) -> u32 {
        match self {
            Self::Spawn { req, .. }
            | Self::Kill { req, .. }
            | Self::Release { req, .. }
            | Self::HoldListener { req }
            | Self::UpgradeKrilld { req, .. }
            | Self::UpgradeHolder { req, .. }
            | Self::Adopt { req, .. }
            | Self::Terminate { req, .. } => *req,
        }
    }

    /// 프레임 전체를 만든다. [`MAX_FRAME`]을 넘으면 `BadLength`.
    pub fn encode(&self) -> Result<Vec<u8>, Error> {
        let mut w = Writer::new();
        w.u32(self.req());
        let kind = match self {
            Self::Spawn {
                id,
                argv,
                env,
                cwd,
                size,
                ..
            } => {
                w.u64(id.0);
                w.u32(len32(argv.len())?);
                for a in argv {
                    w.bytes(a)?;
                }
                w.u32(len32(env.len())?);
                for (k, v) in env {
                    w.bytes(k)?;
                    w.bytes(v)?;
                }
                w.bytes(cwd)?;
                w.u16(size.cols);
                w.u16(size.rows);
                kind::SPAWN
            }
            Self::Kill { id, signal, .. } => {
                w.u64(id.0);
                w.i32(*signal);
                kind::KILL
            }
            Self::Release { id, .. } => {
                w.u64(id.0);
                kind::RELEASE
            }
            Self::HoldListener { .. } => kind::HOLD_LISTENER,
            Self::UpgradeKrilld { path, .. } => {
                w.bytes(path)?;
                kind::UPGRADE_KRILLD
            }
            Self::UpgradeHolder { path, .. } => {
                w.bytes(path)?;
                kind::UPGRADE_HOLDER
            }
            Self::Adopt { id, pid, .. } => {
                w.u64(id.0);
                w.u32(*pid);
                kind::ADOPT
            }
            Self::Terminate { id, .. } => {
                w.u64(id.0);
                kind::TERMINATE
            }
        };
        w.finish(kind, self.fds())
    }

    pub fn decode(f: Frame<'_>) -> Result<Self, Error> {
        let mut r = Reader(f.body);
        let req = r.u32()?;
        let m = match f.kind {
            kind::SPAWN => {
                let id = SessionId(r.u64()?);
                let mut argv = Vec::new();
                for _ in 0..r.u32()? {
                    argv.push(r.bytes()?);
                }
                let mut env = Vec::new();
                for _ in 0..r.u32()? {
                    env.push((r.bytes()?, r.bytes()?));
                }
                let cwd = r.bytes()?;
                let size = WinSize {
                    cols: r.u16()?,
                    rows: r.u16()?,
                };
                Self::Spawn {
                    req,
                    id,
                    argv,
                    env,
                    cwd,
                    size,
                }
            }
            kind::KILL => Self::Kill {
                req,
                id: SessionId(r.u64()?),
                signal: r.i32()?,
            },
            kind::RELEASE => Self::Release {
                req,
                id: SessionId(r.u64()?),
            },
            kind::HOLD_LISTENER => Self::HoldListener { req },
            kind::UPGRADE_KRILLD => Self::UpgradeKrilld {
                req,
                path: r.bytes()?,
            },
            kind::UPGRADE_HOLDER => Self::UpgradeHolder {
                req,
                path: r.bytes()?,
            },
            kind::ADOPT => Self::Adopt {
                req,
                id: SessionId(r.u64()?),
                pid: r.u32()?,
            },
            kind::TERMINATE => Self::Terminate {
                req,
                id: SessionId(r.u64()?),
            },
            k => return Err(Error::UnknownKind(k)),
        };
        check_fds(f, m.fds())?;
        Ok(m)
    }
}

impl FromHolder {
    /// 이 메시지에 붙는 fd 수.
    pub fn fds(&self) -> u8 {
        u8::from(matches!(
            self,
            Self::Session { .. } | Self::Listener | Self::Spawned { .. }
        ))
    }

    /// 프레임 전체를 만든다.
    pub fn encode(&self) -> Result<Vec<u8>, Error> {
        let mut w = Writer::new();
        let kind = match self {
            Self::Hello {
                version,
                holder_pid,
            } => {
                w.u16(*version);
                w.u32(*holder_pid);
                kind::HELLO
            }
            Self::Session { id, pid, status } => {
                w.u64(id.0);
                w.u32(*pid);
                match status {
                    None => w.u8(0),
                    Some(s) => {
                        w.u8(1);
                        w.i32(*s);
                    }
                }
                kind::SESSION
            }
            Self::Listener => kind::LISTENER,
            Self::Ready => kind::READY,
            Self::Spawned { req, id, pid } => {
                w.u32(*req);
                w.u64(id.0);
                w.u32(*pid);
                kind::SPAWNED
            }
            Self::Exited { id, status } => {
                w.u64(id.0);
                w.i32(*status);
                kind::EXITED
            }
            Self::Done { req } => {
                w.u32(*req);
                kind::DONE
            }
            Self::Failed { req, errno } => {
                w.u32(*req);
                w.i32(*errno);
                kind::FAILED
            }
        };
        w.finish(kind, self.fds())
    }

    pub fn decode(f: Frame<'_>) -> Result<Self, Error> {
        let mut r = Reader(f.body);
        let m = match f.kind {
            kind::HELLO => Self::Hello {
                version: r.u16()?,
                holder_pid: r.u32()?,
            },
            kind::SESSION => Self::Session {
                id: SessionId(r.u64()?),
                pid: r.u32()?,
                status: match r.u8()? {
                    0 => None,
                    1 => Some(r.i32()?),
                    t => return Err(Error::BadTag(t)),
                },
            },
            kind::LISTENER => Self::Listener,
            kind::READY => Self::Ready,
            kind::SPAWNED => Self::Spawned {
                req: r.u32()?,
                id: SessionId(r.u64()?),
                pid: r.u32()?,
            },
            kind::EXITED => Self::Exited {
                id: SessionId(r.u64()?),
                status: r.i32()?,
            },
            kind::DONE => Self::Done { req: r.u32()? },
            kind::FAILED => Self::Failed {
                req: r.u32()?,
                errno: r.i32()?,
            },
            k => return Err(Error::UnknownKind(k)),
        };
        check_fds(f, m.fds())?;
        Ok(m)
    }
}

fn check_fds(f: Frame<'_>, want: u8) -> Result<(), Error> {
    if f.fds == want {
        Ok(())
    } else {
        Err(Error::FdCount {
            kind: f.kind,
            got: f.fds,
        })
    }
}

fn len32(n: usize) -> Result<u32, Error> {
    u32::try_from(n).map_err(|_| Error::BadLength(u32::MAX))
}

struct Writer(Vec<u8>);

impl Writer {
    fn new() -> Self {
        Self(vec![0; HEADER])
    }
    fn u8(&mut self, v: u8) {
        self.0.push(v);
    }
    fn u16(&mut self, v: u16) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }
    fn u32(&mut self, v: u32) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }
    fn i32(&mut self, v: i32) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }
    fn u64(&mut self, v: u64) {
        self.0.extend_from_slice(&v.to_le_bytes());
    }
    fn bytes(&mut self, v: &[u8]) -> Result<(), Error> {
        self.u32(len32(v.len())?);
        self.0.extend_from_slice(v);
        Ok(())
    }
    fn finish(mut self, kind: u8, fds: u8) -> Result<Vec<u8>, Error> {
        let n = self.0.len().saturating_sub(4);
        if n > MAX_FRAME {
            return Err(Error::BadLength(u32::try_from(n).unwrap_or(u32::MAX)));
        }
        let mut head = len32(n)?.to_le_bytes().to_vec();
        head.extend_from_slice(&[kind, fds]);
        if let Some(h) = self.0.get_mut(..HEADER) {
            h.copy_from_slice(&head);
        }
        Ok(self.0)
    }
}

struct Reader<'a>(&'a [u8]);

impl Reader<'_> {
    fn take<const N: usize>(&mut self) -> Result<[u8; N], Error> {
        let (head, rest) = self.0.split_first_chunk::<N>().ok_or(Error::Truncated)?;
        self.0 = rest;
        Ok(*head)
    }
    fn u8(&mut self) -> Result<u8, Error> {
        Ok(u8::from_le_bytes(self.take()?))
    }
    fn u16(&mut self) -> Result<u16, Error> {
        Ok(u16::from_le_bytes(self.take()?))
    }
    fn u32(&mut self) -> Result<u32, Error> {
        Ok(u32::from_le_bytes(self.take()?))
    }
    fn i32(&mut self) -> Result<i32, Error> {
        Ok(i32::from_le_bytes(self.take()?))
    }
    fn u64(&mut self) -> Result<u64, Error> {
        Ok(u64::from_le_bytes(self.take()?))
    }
    fn bytes(&mut self) -> Result<Vec<u8>, Error> {
        let n = usize::try_from(self.u32()?).map_err(|_| Error::Truncated)?;
        let (head, rest) = self.0.split_at_checked(n).ok_or(Error::Truncated)?;
        self.0 = rest;
        Ok(head.to_vec())
    }
}

#[cfg(test)]
mod tests;
