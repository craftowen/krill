//! 다중 클라이언트 역할·크기(MK-050). `FEAT_ROLES`를 협상한 연결에서만 쓴다.
//!
//! Hello 뒤 Attach/제어 요청 앞에 `Set`을 보내거나, Attach 뒤 역할을 바꿀 수 있다.
//! 기본은 구 클라이언트와 같은 interactive다. observer는 입력·크기·포커스·변경 제어 요청을 거부한다.
//! `Resize`는 자기 뷰포트만 기억하며, 가장 최근 입력/`Focus`한 interactive의 크기만 PTY에 적용한다.
//! 입력 프레임은 세션별로 직렬화한다(서로 다른 연결의 동시 도착은 데몬 잠금 획득 순서).
//! observer의 `Input`에는 `InputAck(Denied)`로 답하며 PTY에 쓰지 않는다.
//! 제어 연결에서는 `Set`과 읽기 요청만 가능하고, 세션 지정 `control::Request::Resize`는 포커스도 겸한다.
//! 소유자가 끊기거나 observer로 바뀌면 남은 interactive 중 마지막 활동자의 크기로 돌아간다.
//! 활동자가 없으면 마지막 PTY 크기를 유지한다. 응답은 요청마다 하나이며 같은 연결의 요청 순서를 따른다.
//! 뷰포트를 보고하지 않은 구 클라이언트는 Attach 시점의 PTY 크기를 기억한다.
//! 기존 Attach·Input DTO는 바꾸지 않는다. 역할 응답은 협상한 연결에만 보낸다.

use serde::{Deserialize, Serialize};

use crate::screen::Msg;

pub const FEAT_ROLES: u64 = 1 << 12;

pub mod kind {
    pub const ROLE: u8 = 0x17;
    pub const ROLE_ACK: u8 = 0x2c;
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Role {
    #[default]
    Interactive,
    Observer,
    /// `FEAT_MOBILE` 확장. 기존 두 variant의 고정 바이트는 유지한다.
    InteractiveWithSize {
        owns_size: bool,
    },
}

impl Role {
    pub const fn owns_size(self) -> bool {
        matches!(
            self,
            Self::Interactive | Self::InteractiveWithSize { owns_size: true }
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Request {
    Set(Role),
    Resize { cols: u16, rows: u16 },
    Focus,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Ack {
    Applied,
    Denied,
    Invalid,
    Closed,
}

impl Msg for Request {
    const KIND: u8 = kind::ROLE;
}

impl Msg for Ack {
    const KIND: u8 = kind::ROLE_ACK;
}

#[cfg(test)]
mod tests;
