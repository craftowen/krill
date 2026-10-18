//! hook 이벤트 와이어·spool 포맷(MK-038, ARCHITECTURE §6). `krill hook <agent>`가 에이전트 hook의 stdin JSON을 krilld에
//! 넘기고, 넘기지 못하면 spool 파일에 쌓는다. krilld 쪽 수신·spool 수거·정규화는 MK-039(`krilld::status`).
//!
//! # 소켓
//!
//! 1. `krill hook`은 [`ClientRole::Hook`](crate::ipc::ClientRole), feature [`FEAT_HOOK`]만 걸고 핸드셰이크한다. 데몬은
//!    띄우지 않는다(50ms 예산).
//! 2. 협상 결과에 [`FEAT_HOOK`]이 없으면(이 기능 전의 krilld) 보내지 않고 spool로 간다.
//! 3. [`HookEvent`] 프레임([`kind::EVENT`]) 하나를 보내고 [`kind::ACK`](body 없음)를 기다린다. 연결 하나에 이벤트
//!    하나다.
//! 4. ACK가 예산 안에 오지 않으면 spool에도 쓴다. 그래서 krilld는 같은 이벤트를 두 번 받을 수 있고
//!    [`HookEvent::id`]로 중복을 없앤다.
//!
//! # spool
//!
//! `<상태 디렉터리>/`[`SPOOL_FILE`](0600). 내용은 소켓으로 보낼 [`HookEvent`] 프레임을 그대로 이어 붙인 것이다
//! ([`crate::ipc`] 프레임 형식). 쓰는 쪽([`append_spool`])은 파일 flock을 잡고 `O_APPEND`로 프레임 하나를 한 번의
//! `write`로 쓴다. 수거하는 쪽은 같은 flock 아래에서 다 읽고 길이를 0으로 자른다. [`read_spool`]은 경계가 온전한
//! 손상 레코드만 건너뛰고, 잘린 꼬리·잘못된 길이에서는 멈춘다. 파일이 [`SPOOL_MAX`]를 넘으면 더 쓰지 않는다.

use crate::os::fs::{DirBuilderExt as _, OpenOptionsExt as _};
use std::fs::{DirBuilder, File, OpenOptions, TryLockError};
use std::io::{self, Write as _};
use std::path::Path;
use std::thread;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::ipc::{self, Error, NAME_MAX};

/// hook 이벤트를 받는 krilld만 거는 feature 비트.
pub const FEAT_HOOK: u64 = 1 << 3;
/// [`HookEvent::payload`] 최대 바이트. 넘는 stdin은 앞부분만 보내고 [`Input::Truncated`]로 표시한다.
pub const MAX_PAYLOAD: usize = 256 * 1024;
/// 상태 디렉터리 안 spool 파일 이름.
pub const SPOOL_FILE: &str = "hook.spool";
/// spool 파일이 이 크기 이상이면 새 이벤트를 버린다.
pub const SPOOL_MAX: u64 = 16 << 20;
/// spool flock을 다시 시도하기 전 쉬는 시간.
const LOCK_POLL: Duration = Duration::from_millis(1);

/// [`FEAT_HOOK`]을 협상했을 때만 쓰는 kind. 값은 영구다(ARCHITECTURE §4.3 등록표).
pub mod kind {
    /// `krill hook` → krilld: [`super::HookEvent`].
    pub const EVENT: u8 = 0x14;
    /// krilld → `krill hook`: 이벤트를 받았다(body 없음).
    pub const ACK: u8 = 0x28;
}

/// 에이전트 hook 호출 하나. 필드는 끝에만 붙인다([`crate::ipc`] 호환 규칙).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HookEvent {
    /// 중복 제거 키(소켓과 spool 양쪽으로 갈 수 있다).
    pub id: EventId,
    /// `krill hook <agent>`의 에이전트 이름([`NAME_MAX`] 바이트 이하). 해석은 krilld가 한다.
    pub agent: String,
    /// hook 프로세스의 부모(hook을 부른 에이전트) pid. spool에서 수거한 이벤트에는 피어 PID가 없어서 세션을 찾는
    /// 단서로 둔다.
    pub ppid: u32,
    pub input: Input,
    /// stdin 원본(JSON이라고 가정하지만 검사하지 않는다). [`MAX_PAYLOAD`] 바이트 이하.
    pub payload: Vec<u8>,
}

/// 이벤트 식별자: hook 프로세스 pid와 시작 시각. 같은 pid가 같은 나노초에 두 번 뜰 수 없다.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct EventId {
    pub pid: u32,
    /// UNIX epoch 나노초.
    pub unix_ns: u64,
}

/// stdin을 어디까지 읽었는지. variant는 끝에만 붙인다.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Input {
    /// EOF까지 다 읽었다.
    Complete,
    /// [`MAX_PAYLOAD`]를 넘어 앞부분만 담았다(JSON으로 읽히지 않을 수 있다).
    Truncated,
    /// 예산 안에 stdin이 닫히지 않았다. payload는 비어 있다.
    TimedOut,
}

impl HookEvent {
    /// 프레임 전체를 만든다. 이름·payload가 상한을 넘으면 `TooLong`.
    pub fn encode(&self) -> Result<Vec<u8>, Error> {
        self.check()?;
        ipc::encode(kind::EVENT, self)
    }

    pub fn decode(kind: u8, body: &[u8]) -> Result<Self, Error> {
        let e: Self = ipc::decode(kind::EVENT, kind, body)?;
        e.check()?;
        Ok(e)
    }

    fn check(&self) -> Result<(), Error> {
        for (len, max) in [
            (self.agent.len(), NAME_MAX),
            (self.payload.len(), MAX_PAYLOAD),
        ] {
            if len > max {
                return Err(Error::TooLong(len));
            }
        }
        Ok(())
    }
}

/// [`kind::ACK`] 프레임 전체.
pub fn ack() -> Vec<u8> {
    vec![1, 0, 0, 0, kind::ACK]
}

/// spool 파일에 `frame`(인코딩한 [`HookEvent`] 프레임) 하나를 붙인다. 디렉터리가 없으면 0700으로 만든다.
///
/// flock은 `deadline`까지만 기다린다. 넘으면 락 없이 붙인다(`O_APPEND` 한 번의 `write`라 다른 쓰는 쪽과 섞이지
/// 않는다).
/// ponytail: 락 없이 붙인 순간 수거가 겹치면 그 이벤트는 잘려 나갈 수 있다. 수거가 락을 수 ms 넘게 쥐어
/// 실제로 보이면 수거를 rename 방식으로 바꾼다.
///
/// # Errors
///
/// 파일이 [`SPOOL_MAX`] 이상이면 `Other`, 그 밖에는 열기·쓰기 실패.
pub fn append_spool(path: &Path, frame: &[u8], deadline: Instant) -> io::Result<()> {
    if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
        DirBuilder::new().recursive(true).mode(0o700).create(dir)?;
    }
    let mut f = OpenOptions::new()
        .append(true)
        .create(true)
        .mode(0o600)
        .open(path)?;
    lock_until(&f, deadline)?;
    if f.metadata()?.len() >= SPOOL_MAX {
        return Err(io::Error::other("hook spool is full"));
    }
    f.write_all(frame)
}

/// `f`의 flock을 `deadline`까지 시도한다. 시간이 다 되면 락 없이 돌아온다.
fn lock_until(f: &File, deadline: Instant) -> io::Result<()> {
    loop {
        match f.try_lock() {
            Err(TryLockError::WouldBlock) if Instant::now() < deadline => thread::sleep(LOCK_POLL),
            Ok(()) | Err(TryLockError::WouldBlock) => return Ok(()),
            Err(TryLockError::Error(e)) => return Err(e),
        }
    }
}

/// spool 내용을 앞에서부터 읽는다. 경계를 읽은 레코드의 디코드 실패는 그 레코드만 버린다. 잘린 꼬리·잘못된
/// 길이는 다음 경계를 알 수 없어 거기서 멈춘다(패닉 없음).
pub fn read_spool(mut bytes: &[u8]) -> Vec<HookEvent> {
    let mut out = Vec::new();
    while let Ok(Some((k, body))) = ipc::read_frame(&mut bytes) {
        if let Ok(e) = HookEvent::decode(k, &body) {
            out.push(e);
        }
    }
    out
}

#[cfg(test)]
mod tests;
