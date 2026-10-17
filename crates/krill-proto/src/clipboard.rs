//! OSC 52 클립보드 쓰기 통지(MK-033). `FEAT_CLIPBOARD` 협상한 화면 연결에만 보낸다.
//!
//! 프로그램이 살아 있는 출력으로 OSC 52 쓰기를 보내면 krilld가 마지막 값 하나를 [`Clipboard`]로 알리고, 앱이 시스템
//! 클립보드에 쓴다. 붙기 전에 쓴 값과 재생한 바이트의 쓰기는 보내지 않는다. OSC 52 **읽기는 아무도 답하지 않는다**(보안,
//! MK-008 19번). 사이에 여러 번 쓰면 마지막 값만 간다(클립보드는 마지막 값만 의미가 있다).

use serde::{Deserialize, Serialize};

pub const FEAT_CLIPBOARD: u64 = 1 << 33;
/// 데몬→클라.
pub const CLIPBOARD: u8 = 0x62;
/// 보내는 텍스트 최대 바이트. 넘는 쓰기는 krilld가 버린다(잘라서 넣으면 다른 값이 된다).
pub const MAX_CLIPBOARD: usize = 1 << 19;

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Clipboard {
    pub text: String,
}

impl crate::screen::Msg for Clipboard {
    const KIND: u8 = CLIPBOARD;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::screen::Msg as _;

    #[test]
    fn fixed_frame_and_roundtrip() {
        let m = Clipboard {
            text: "hi".to_owned(),
        };
        assert_eq!(m.encode().unwrap(), [4, 0, 0, 0, CLIPBOARD, 2, b'h', b'i']);
        assert_eq!(Clipboard::decode(CLIPBOARD, &[2, b'h', b'i']).unwrap(), m);
    }
}
