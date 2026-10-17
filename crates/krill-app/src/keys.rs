//! 키 → PTY 바이트(OWE-490). 기본 xterm 시퀀스만: Enter·Backspace·Tab·Esc·방향키·Home/End·PgUp/PgDn·Delete·
//! Ctrl-문자. 글자 키는 여기 오지 않고 IME 확정 텍스트(`krill_term::ime`)로 간다. 협상된 kitty keyboard·SGR 마우스는 `tui`(MK-029).
//!
//! 이름은 gpui `Keystroke::key` 값이다(소문자 글자, "enter" 등).

pub mod tui;

/// 키 하나의 PTY 바이트. 글자 키(IME가 맡음)나 모르는 키는 `None`.
// 기본 모드 전용 헬퍼. 실제 셸 입력은 `tui::key`가 협상된 모드에 따라 이 함수를 사용한다.
pub fn encode(key: &str, ctrl: bool, shift: bool) -> Option<Vec<u8>> {
    // AS-03: F1–F4 레거시 인코딩(수식키 없으면 SS3 `ESC O P..S`, 있으면 `CSI 1;m P..S`).
    if let Some(letter) = fkey_letter(key) {
        if ctrl || shift {
            let modifier = 1 + u8::from(shift) + 4 * u8::from(ctrl);
            return Some(format!("\x1b[1;{modifier}{letter}").into_bytes());
        }
        return Some(format!("\x1bO{letter}").into_bytes());
    }
    if ctrl {
        return ctrl_byte(key).map(|b| vec![b]);
    }
    let fixed: &[u8] = match key {
        "enter" => b"\r",
        "backspace" => b"\x7f",
        "tab" if shift => b"\x1b[Z",
        "tab" => b"\t",
        "escape" => b"\x1b",
        "up" => b"\x1b[A",
        "down" => b"\x1b[B",
        "right" => b"\x1b[C",
        "left" => b"\x1b[D",
        "home" => b"\x1b[H",
        "end" => b"\x1b[F",
        "pageup" => b"\x1b[5~",
        "pagedown" => b"\x1b[6~",
        "delete" => b"\x1b[3~",
        _ => return None,
    };
    Some(fixed.to_vec())
}

/// F1–F4의 SS3·CSI 문자(`P`·`Q`·`R`·`S`). F5–F12는 `~` 계열이라 해당 없음.
fn fkey_letter(key: &str) -> Option<char> {
    match key {
        "f1" => Some('P'),
        "f2" => Some('Q'),
        "f3" => Some('R'),
        "f4" => Some('S'),
        _ => None,
    }
}

/// Ctrl-문자: 글자는 `& 0x1f`, `space`·`@`는 NUL, `[ \ ] ^ _`는 0x1b~0x1f, `?`는 DEL.
/// AS-18: `Ctrl+/`(0x1F, readline·emacs 실행 취소)와 `Ctrl+2`..=`Ctrl+8`도 보낸다.
/// `Ctrl+1`·`Ctrl+9`·`Ctrl+0`은 셸 탭 전환 단축키라 `None`을 유지한다.
fn ctrl_byte(key: &str) -> Option<u8> {
    let mut chars = key.chars();
    let c = match (chars.next(), chars.next()) {
        (Some(c), None) => c,
        _ if key == "space" => return Some(0),
        _ => return None,
    };
    match c {
        'a'..='z' | '@' | '['..='_' => u8::try_from(c).ok().map(|b| b & 0x1f),
        '?' | '8' => Some(0x7f),
        '/' | '7' => Some(0x1f),
        '2' => Some(0x00),
        '3' => Some(0x1b),
        '4' => Some(0x1c),
        '5' => Some(0x1d),
        '6' => Some(0x1e),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn specials_and_ctrl() {
        let e = |k, ctrl, shift| encode(k, ctrl, shift);
        assert_eq!(e("enter", false, false).unwrap(), b"\r");
        assert_eq!(e("backspace", false, false).unwrap(), b"\x7f");
        assert_eq!(e("tab", false, true).unwrap(), b"\x1b[Z");
        assert_eq!(e("left", false, false).unwrap(), b"\x1b[D");
        assert_eq!(e("c", true, false).unwrap(), [3]);
        assert_eq!(e("d", true, true).unwrap(), [4]);
        assert_eq!(e("space", true, false).unwrap(), [0]);
        assert_eq!(e("[", true, false).unwrap(), [0x1b]);
        // 글자 키는 IME 확정 텍스트로 간다.
        assert_eq!(e("a", false, false), None);
        assert_eq!(e("f5", true, false), None);
        assert_eq!(e("한", true, false), None);
        // 셸 탭 전환 단축키(Ctrl+Tab, Ctrl+PageUp/PageDown, Ctrl+1..9)는 PTY 바이트로 변환하지 않는다.
        assert_eq!(e("tab", true, false), None);
        assert_eq!(e("tab", true, true), None);
        assert_eq!(e("pageup", true, false), None);
        assert_eq!(e("pagedown", true, false), None);
        assert_eq!(e("1", true, false), None);
    }

    /// AS-03: F1–F4 레거시 인코딩(SS3, 수식키는 `CSI 1;m`).
    #[test]
    fn f1_to_f4_legacy_sequences() {
        let e = |k, ctrl, shift| encode(k, ctrl, shift);
        assert_eq!(e("f1", false, false).unwrap(), b"\x1bOP");
        assert_eq!(e("f2", false, false).unwrap(), b"\x1bOQ");
        assert_eq!(e("f3", false, false).unwrap(), b"\x1bOR");
        assert_eq!(e("f4", false, false).unwrap(), b"\x1bOS");
        assert_eq!(e("f1", false, true).unwrap(), b"\x1b[1;2P");
        assert_eq!(e("f1", true, false).unwrap(), b"\x1b[1;5P");
        assert_eq!(e("f4", true, true).unwrap(), b"\x1b[1;6S");
        // F5–F12는 그대로 `~` 계열(tui 분기)이라 여기서는 `None`.
        assert_eq!(e("f5", false, false), None);
    }

    /// AS-18: Ctrl+/·Ctrl+2..8. Ctrl+1·9·0은 셸 단축키라 `None` 유지.
    #[test]
    fn ctrl_slash_and_digits() {
        let e = |k| encode(k, true, false);
        assert_eq!(e("/").unwrap(), [0x1f]);
        assert_eq!(e("2").unwrap(), [0x00]);
        assert_eq!(e("3").unwrap(), [0x1b]);
        assert_eq!(e("8").unwrap(), [0x7f]);
        assert_eq!(e("1"), None);
        assert_eq!(e("9"), None);
        assert_eq!(e("0"), None);
    }
}
