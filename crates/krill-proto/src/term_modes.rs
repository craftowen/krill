//! MK-029 입력 모드. `FEAT_TERM_MODES` 협상한 화면 연결에만 `Attached` 뒤 초기값과 변경값을 보낸다.
//! 화면과 같은 잠금에서 캡처하며 재접속 때는 revision 생략 여부와 무관하게 초기값을 다시 보낸다.
//! 엔진의 비트 번호를 노출하지 않는다. 모르는 비트는 무시하며 DTO 필드와 비트 번호는 영구다.

use serde::{Deserialize, Serialize};

pub const FEAT_TERM_MODES: u64 = 1 << 13;
pub const TERM_MODES: u8 = 0x2b;
/// 화면 지우기는 입력 바이트 없이 엔진 화면만 바꾼다. 기존 `ClearScrollback` 계약과 분리한다.
pub const FEAT_CLEAR_VIEWPORT: u64 = 1 << 21;
pub const CLEAR_VIEWPORT: u8 = 0x47;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClearViewport;

impl crate::screen::Msg for ClearViewport {
    const KIND: u8 = CLEAR_VIEWPORT;

    fn decode(kind: u8, body: &[u8]) -> Result<Self, crate::ipc::Error> {
        if kind != Self::KIND {
            return Err(crate::ipc::Error::UnexpectedKind {
                want: Self::KIND,
                got: kind,
            });
        }
        if !body.is_empty() {
            return Err(crate::ipc::Error::BadLength(
                u64::try_from(body.len()).unwrap_or(u64::MAX),
            ));
        }
        Ok(Self)
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Modes {
    /// kitty progressive enhancement 플래그(하위 5비트).
    pub kitty: u8,
    pub flags: u16,
}

impl Modes {
    pub const APP_CURSOR: u16 = 1;
    pub const MOUSE_CLICK: u16 = 1 << 1;
    pub const MOUSE_DRAG: u16 = 1 << 2;
    pub const MOUSE_MOTION: u16 = 1 << 3;
    pub const SGR_MOUSE: u16 = 1 << 4;
    pub const BRACKETED_PASTE: u16 = 1 << 5;
    pub const ALT_SCREEN: u16 = 1 << 6;
    pub const ALTERNATE_SCROLL: u16 = 1 << 7;

    pub fn contains(self, flag: u16) -> bool {
        self.flags & flag == flag
    }

    pub fn mouse(self) -> bool {
        self.flags & (Self::MOUSE_CLICK | Self::MOUSE_DRAG | Self::MOUSE_MOTION) != 0
    }
}

impl crate::screen::Msg for Modes {
    const KIND: u8 = TERM_MODES;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::screen::Msg as _;

    #[test]
    fn clear_viewport_frozen_frame() {
        assert_eq!(
            ClearViewport.encode().unwrap(),
            [1, 0, 0, 0, CLEAR_VIEWPORT]
        );
        assert_eq!(
            ClearViewport::decode(CLEAR_VIEWPORT, &[]).unwrap(),
            ClearViewport
        );
        assert!(ClearViewport::decode(CLEAR_VIEWPORT, &[1]).is_err());
    }

    #[test]
    fn fixed_frame_and_roundtrip() {
        let m = Modes {
            kitty: 7,
            flags: Modes::APP_CURSOR | Modes::SGR_MOUSE,
        };
        assert_eq!(m.encode().unwrap(), [3, 0, 0, 0, TERM_MODES, 7, 17]);
        assert_eq!(Modes::decode(TERM_MODES, &[7, 17]).unwrap(), m);
        assert_eq!(
            Modes::decode(TERM_MODES, &[0, 0]).unwrap(),
            Modes::default()
        );
    }
}
