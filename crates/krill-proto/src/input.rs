//! 입력 전달과 끊긴 동안 보류(MK-026, T2 #15). [`crate::ipc`] v1 프레임 위에서, 협상한 feature에 [`FEAT_INPUT`]이
//! 있을 때만 쓰는 kind다. 세션 스트림 연결([`crate::screen`]의 `Attach` 뒤)에서 보내므로 대상 세션은 그 연결의 세션이다.
//!
//! # 와이어
//!
//! - 클라이언트 → krilld [`Input`]: 입력 ID와 PTY에 쓸 바이트([`MAX_INPUT`] 이하).
//! - krilld → 클라이언트 [`InputAck`]: 그 입력을 PTY에 다 썼거나(`Applied`), 세션이 끝나 쓰지 않았다(`Closed`).
//!   krilld는 받은 순서대로 처리하고 입력마다 정확히 한 번 답한다.
//!
//! # 보장 (클라이언트 [`Journal`])
//!
//! - **중복 실행 0**: 한 번 보낸 입력은 다시 보내지 않는다(최대 한 번). 보냈지만 ack 전에 연결이 끊긴 입력은
//!   krilld가 썼는지 알 수 없으므로 [`Fate::Uncertain`]으로 화면에 알린다.
//! - **유실 0**: 끊긴 동안 친 입력은 보류했다가 같은 세션에 다시 붙으면 한 번 보낸다. 보내지 못하고 버리는 입력은
//!   모두 [`Notice`]로 알린다(조용한 유실 없음).
//! - **새 셸 incarnation에는 보내지 않는다**: 보류 입력은 친 세션에만 간다. 다시 붙은 세션이 다르면(세션이 사라졌거나
//!   칸이 새 셸로 바뀜) [`Fate::NewSession`]으로 버린다. krilld는 새 프로세스에 이전 [`crate::SessionId`]를 다시 쓰지
//!   않는다(세션 id = 셸 incarnation).
//!   명시적 계정 전환은 UI가 같은 대화의 새 계정 세션을 확인한 뒤, 전환 대기 중의 **미전송** 입력만
//!   승계할 수 있다. 이미 전송해 적용 여부를 모르는 입력은 이 경우에도 재전송하지 않는다.
//! - 연결 정체성은 krilld 세대(`instance_id`)가 아니라 세션이다. krilld가 재기동해도 holder가 PTY를 들고 있어
//!   세션은 같다(perf-first #5).
//!
//! # 시간 (잠자기)
//!
//! - 하트비트는 따로 없다. UDS는 krilld가 죽으면 바로 EOF가 난다. 멈춘 krilld는 ack가 [`PROBE`] 넘게 오지 않는
//!   것으로 안다([`Journal::stalled`]). 유휴 중 보내는 것은 없다(perf-first #13).
//! - [`PROBE`]는 [`Now::awake`](`Instant`, macOS에서 `CLOCK_UPTIME_RAW`라 잠자기 중 멈춤)로 재서 깨어난 직후
//!   오탐하지 않는다. 보류 유예 [`GRACE`]는 [`Now::wall`]로 재서 잠자기 시간도 친다(자고 일어나 오래된 키가 실행되지
//!   않게).
//!
//! # 호환
//!
//! [`crate::screen`]과 같다. `input/tests.rs`의 고정 바이트가 바뀌면 호환 위반이다.

use std::collections::VecDeque;
use std::time::{Duration, Instant, SystemTime};

use serde::{Deserialize, Serialize};

use crate::screen::Msg;

/// 이 모듈의 kind를 쓸 수 있다는 feature 비트.
pub const FEAT_INPUT: u64 = 1 << 2;
/// [`Input`] 한 프레임의 최대 바이트. 큰 붙여넣기는 [`Journal`]이 나눈다.
pub const MAX_INPUT: usize = 64 * 1_024;
/// 끊긴 동안 세션 하나에 보류하는 최대 바이트. 넘는 입력은 받지 않고 [`Fate::Full`]로 알린다.
/// 설계값(perf-first #5, 측정 근거 없음).
pub const HOLD_MAX: usize = 64 * 1_024;
/// 보류 입력의 유예. 친 지 이만큼 지난 입력은 다시 붙어도 보내지 않는다([`Fate::Expired`]). Orca와 같은 5분(설계값).
pub const GRACE: Duration = Duration::from_mins(5);
/// 보낸 입력의 ack를 이만큼 못 받으면 krilld가 멈춘 것으로 본다. 설계값(perf-first #13 "입력 후 2초").
pub const PROBE: Duration = Duration::from_secs(2);

/// 이 모듈의 kind. 값은 영구다.
pub mod kind {
    /// 클라이언트 → krilld: [`super::Input`].
    pub const INPUT: u8 = 0x13;
    /// krilld → 클라이언트: [`super::InputAck`].
    pub const INPUT_ACK: u8 = 0x27;
}

/// 클라이언트가 연결 단위로 매기는 입력 번호(1부터 오름).
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize,
)]
pub struct InputId(pub u64);

/// [`kind::INPUT`]: PTY에 쓸 바이트.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Input {
    pub id: InputId,
    pub bytes: Vec<u8>,
}

/// [`InputAck`]의 결과. variant는 끝에만 붙인다.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum InputResult {
    /// PTY에 다 썼다.
    Applied,
    /// 세션이 끝나 쓰지 않았다.
    Closed,
    /// 역할 권한으로 입력을 거부했다.
    Denied,
}

/// [`kind::INPUT_ACK`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct InputAck {
    pub id: InputId,
    pub result: InputResult,
}

impl Msg for Input {
    const KIND: u8 = kind::INPUT;
}
impl Msg for InputAck {
    const KIND: u8 = kind::INPUT_ACK;
}

/// 지금 시각 둘(모듈 문서 "시간").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Now {
    /// 깨어 있는 시간만 가는 시계.
    pub awake: Instant,
    /// 벽시계(잠자기 시간 포함, 되돌아갈 수 있음).
    pub wall: SystemTime,
}

impl Now {
    pub fn get() -> Self {
        Self {
            awake: Instant::now(),
            wall: SystemTime::now(),
        }
    }
}

/// 보내지 못했거나 결과를 모르는 입력의 처지.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fate {
    /// 보냈지만 적용 확인 전에 연결이 끊겼다. 다시 보내지 않는다.
    Uncertain,
    /// [`GRACE`]가 지나 보내지 않았다.
    Expired,
    /// 다시 붙은 세션이 친 세션이 아니라 보내지 않았다.
    NewSession,
    /// 세션이 끝나 krilld가 쓰지 않았다.
    SessionClosed,
    /// 보류 상한([`HOLD_MAX`])을 넘어 받지 않았다.
    Full,
    /// 연결한 데몬이 [`FEAT_INPUT`]을 지원하지 않는다.
    Unsupported,
}

/// 화면에 알릴 입력. `bytes`는 해당 입력을 순서대로 이은 것이다.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Notice {
    pub fate: Fate,
    pub bytes: Vec<u8>,
}

/// 다시 붙은 세션이 입력을 친 세션과 같은지. 호출자가 정한다: 같은 [`crate::SessionId`]에 `Attached`를 받았으면
/// `Same`, `Refused`·다른 세션이면 `Changed`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Incarnation {
    Same,
    Changed,
}

#[derive(Debug, Clone)]
struct Entry {
    id: InputId,
    bytes: Vec<u8>,
    /// 보낸 입력은 보낸 시각, 보류 입력은 친 시각.
    awake: Instant,
    wall: SystemTime,
}

/// 세션 하나(칸 하나)의 입력 장부. 연결 상태에 따라 보낼 [`Input`]을 내주고, 버리거나 결과를 모르는 입력을
/// [`Notice`]로 돌려준다. IO는 하지 않는다. 처음에는 끊긴 상태다([`Journal::relink`]로 붙는다).
#[derive(Debug, Default)]
pub struct Journal {
    last: u64,
    linked: bool,
    /// 보냈고 ack를 기다리는 입력.
    sent: VecDeque<Entry>,
    /// 끊긴 동안 친 입력.
    held: VecDeque<Entry>,
    held_bytes: usize,
}

impl Journal {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn is_linked(&self) -> bool {
        self.linked
    }

    /// 보류 중인 바이트 수(배너 표시용).
    pub fn held_bytes(&self) -> usize {
        self.held_bytes
    }

    pub fn has_pending_input(&self) -> bool {
        self.held_bytes > 0 || !self.sent.is_empty()
    }

    /// Hold new input while an account replacement starts; existing acknowledgements still apply.
    pub fn pause(&mut self) {
        self.linked = false;
    }

    /// 사용자 입력. 붙어 있으면 지금 보낼 프레임들([`MAX_INPUT`]씩 나눔)을, 끊겨 있으면 보류하고 빈 목록을 준다.
    /// 보류 상한을 넘으면 통째로 받지 않고 [`Fate::Full`].
    pub fn input(&mut self, bytes: &[u8], now: Now) -> Result<Vec<Input>, Notice> {
        if self.linked {
            return Ok(bytes
                .chunks(MAX_INPUT)
                .map(|c| self.send(c.to_vec(), now))
                .collect());
        }
        let total = self.held_bytes.saturating_add(bytes.len());
        if total > HOLD_MAX {
            return Err(Notice {
                fate: Fate::Full,
                bytes: bytes.to_vec(),
            });
        }
        self.held_bytes = total;
        self.held.extend(bytes.chunks(MAX_INPUT).map(|c| Entry {
            id: InputId::default(),
            bytes: c.to_vec(),
            awake: now.awake,
            wall: now.wall,
        }));
        Ok(Vec::new())
    }

    fn send(&mut self, bytes: Vec<u8>, now: Now) -> Input {
        self.last = self.last.saturating_add(1);
        let id = InputId(self.last);
        self.sent.push_back(Entry {
            id,
            bytes: bytes.clone(),
            awake: now.awake,
            wall: now.wall,
        });
        Input { id, bytes }
    }

    /// 지원하지 않는 데몬에 연결해 보류 입력을 버렸다는 알림을 만든다.
    pub fn discard_held(&mut self, fate: Fate) -> Option<Notice> {
        self.held_bytes = 0;
        notice(fate, self.held.drain(..))
    }

    /// krilld의 답. 그 입력(과 krilld가 순서대로 이미 처리한 앞 입력)을 장부에서 지운다. 세션이 끝나 쓰지
    /// 않았으면 [`Fate::SessionClosed`]. 모르는 id는 무시한다.
    pub fn ack(&mut self, ack: &InputAck) -> Option<Notice> {
        let n = self.sent.iter().take_while(|e| e.id <= ack.id).count();
        let done: Vec<Entry> = self.sent.drain(..n).collect();
        let acked = done.last().filter(|e| e.id == ack.id)?;
        let fate = match ack.result {
            InputResult::Closed => Fate::SessionClosed,
            InputResult::Denied => Fate::Unsupported,
            InputResult::Applied => return None,
        };
        Some(Notice {
            fate,
            bytes: acked.bytes.clone(),
        })
    }

    /// 연결이 끊겼다. ack를 못 받은 입력은 결과를 모른다([`Fate::Uncertain`], 다시 보내지 않음).
    pub fn unlink(&mut self) -> Option<Notice> {
        self.linked = false;
        notice(Fate::Uncertain, self.sent.drain(..))
    }

    /// 다시 붙었다. 같은 세션이면 유예 안의 보류 입력을 친 순서대로 보낼 프레임으로 준다. 지난 입력과, 세션이
    /// 바뀌었으면 보류 입력 전부를 알림으로 준다.
    pub fn relink(&mut self, session: Incarnation, now: Now) -> (Vec<Input>, Vec<Notice>) {
        self.linked = true;
        self.held_bytes = 0;
        let held: Vec<Entry> = self.held.drain(..).collect();
        if session == Incarnation::Changed {
            return (
                Vec::new(),
                notice(Fate::NewSession, held).into_iter().collect(),
            );
        }
        let (fresh, old): (Vec<Entry>, Vec<Entry>) = held.into_iter().partition(|e| {
            now.wall
                .duration_since(e.wall)
                .map_or(true, |age| age < GRACE)
        });
        let frames = fresh.into_iter().map(|e| self.send(e.bytes, now)).collect();
        (frames, notice(Fate::Expired, old).into_iter().collect())
    }

    /// 보낸 입력 중 가장 오래된 것의 ack가 [`PROBE`] 넘게(깨어 있던 시간으로) 오지 않았다.
    pub fn stalled(&self, now: Now) -> bool {
        self.sent
            .front()
            .is_some_and(|e| now.awake.saturating_duration_since(e.awake) >= PROBE)
    }
}

/// 입력들을 이어 알림 하나로. 없으면 `None`.
fn notice(fate: Fate, entries: impl IntoIterator<Item = Entry>) -> Option<Notice> {
    let bytes: Vec<u8> = entries.into_iter().flat_map(|e| e.bytes).collect();
    (!bytes.is_empty()).then_some(Notice { fate, bytes })
}

#[cfg(test)]
mod tests;
