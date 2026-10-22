//! MK-029. kitty 공식 keyboard-protocol 사양과 xterm SGR 1006 형식으로 독립 구현했다.

use gpui::{Keystroke, Modifiers};
use krill_proto::term_modes::Modes;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Event {
    Press,
    Repeat,
    Release,
}

/// 셸 전역 단축키(`Cmd+*`, `Ctrl+Tab`, `Ctrl+Shift+Tab`, `Ctrl+PageUp/PageDown`, `Ctrl+1..9`)는
/// PTY로 바이트(Press·Repeat·Release 모두)를 보내지 않는다.
pub fn is_shell_shortcut(ks: &Keystroke) -> bool {
    let m = ks.modifiers;
    if m.platform {
        return true;
    }
    if m.control && !m.alt {
        match ks.key.as_str() {
            "tab" => return true,
            "pageup" | "pagedown" | "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9"
                if !m.shift =>
            {
                return true;
            }
            _ => {}
        }
    }
    false
}

/// macOS 줄 편집 관례: Cmd+Backspace = Ctrl+U(커서 왼쪽을 줄 시작까지 지움), Cmd+←/→ = Ctrl+A/Ctrl+E
/// (줄 시작·끝으로 이동). Terminal·iTerm2·Orca와 같다.
fn platform_alias(ks: &Keystroke) -> Option<Keystroke> {
    let m = ks.modifiers;
    if !m.platform || m.control || m.alt || m.shift || m.function {
        return None;
    }
    let key = match ks.key.as_str() {
        "backspace" => "u",
        "left" => "a",
        "right" => "e",
        _ => return None,
    };
    Some(Keystroke {
        modifiers: Modifiers::control(),
        key: key.into(),
        key_char: None,
    })
}

/// 문자 입력은 IME로, 모호한 키·기능 키는 협상한 형식으로 보낸다.
pub fn key(ks: &Keystroke, modes: Modes, event: Event) -> Option<Vec<u8>> {
    if let Some(alias) = platform_alias(ks) {
        return key(&alias, modes, event);
    }
    if is_shell_shortcut(ks) {
        return None;
    }
    let flags = modes.kitty;
    let all = flags & 8 != 0;
    let report_events = flags & 2 != 0;
    let released = event == Event::Release;
    if released && !report_events {
        return None;
    }
    let k = ks.key.as_str();
    let m = ks.modifiers;
    let modified = m.control || m.alt || m.shift;
    let recovery = matches!(k, "enter" | "tab" | "backspace");
    if recovery && !all && (released || !modified) {
        return (!released)
            .then(|| super::encode(k, m.control, m.shift))
            .flatten();
    }
    let modifier = 1 + u8::from(m.shift) + 2 * u8::from(m.alt) + 4 * u8::from(m.control);
    let event_suffix = match (report_events, event) {
        (true, Event::Repeat) => ":2",
        (true, Event::Release) => ":3",
        _ => "",
    };
    let suffix = match k {
        "up" => Some('A'),
        "down" => Some('B'),
        "right" => Some('C'),
        "left" => Some('D'),
        "home" => Some('H'),
        "end" => Some('F'),
        _ => None,
    };
    if let Some(suffix) = suffix {
        let s = if modifier == 1 && event_suffix.is_empty() {
            if modes.contains(Modes::APP_CURSOR) {
                format!("\x1bO{suffix}")
            } else {
                format!("\x1b[{suffix}")
            }
        } else {
            format!("\x1b[1;{modifier}{event_suffix}{suffix}")
        };
        return Some(s.into_bytes());
    }
    let tilde = match k {
        "insert" => Some(2),
        "delete" => Some(3),
        "pageup" => Some(5),
        "pagedown" => Some(6),
        "f5" => Some(15),
        "f6" => Some(17),
        "f7" => Some(18),
        "f8" => Some(19),
        "f9" => Some(20),
        "f10" => Some(21),
        "f11" => Some(23),
        "f12" => Some(24),
        _ => None,
    };
    if let Some(n) = tilde {
        let s = if modifier == 1 && event_suffix.is_empty() {
            format!("\x1b[{n}~")
        } else {
            format!("\x1b[{n};{modifier}{event_suffix}~")
        };
        return Some(s.into_bytes());
    }
    // AS-03: F1–F4는 레거시 SS3(`ESC O P..S`) 계열을 그대로 쓴다(kitty 협상 때도
    // 비표준 `CSI 5736xu`가 아니라 표준 계열). 수식키·이벤트 보고가 있으면 `CSI 1;m P..S`.
    if let Some(letter) = super::fkey_letter(k) {
        let s = if modifier == 1 && event_suffix.is_empty() {
            format!("\x1bO{letter}")
        } else {
            format!("\x1b[1;{modifier}{event_suffix}{letter}")
        };
        return Some(s.into_bytes());
    }
    let code = match k {
        "enter" => 13,
        "tab" => 9,
        "backspace" => 127,
        "escape" => 27,
        "space" => 32,
        _ => {
            let mut chars = k.chars();
            let c = chars.next()?;
            if chars.next().is_some() || !c.is_ascii() {
                return None;
            }
            u32::from(c)
        }
    };
    let printable = (32..127).contains(&code);
    // 문자 release도 report-all 없이 보내지 않는다. press는 IME 확정 경로가 맡는다.
    if printable && !m.control && !m.alt && (!released || !all) {
        return None;
    }
    if flags & (1 | 8) == 0 {
        if released {
            return None;
        }
        let mut legacy = super::encode(k, m.control, m.shift)?;
        if m.alt {
            legacy.insert(0, 0x1b);
        }
        return Some(legacy);
    }
    let alternate = if flags & 4 != 0 && m.shift && printable {
        ks.key_char
            .as_deref()
            .and_then(|t| {
                let mut c = t.chars();
                let first = c.next()?;
                (c.next().is_none() && u32::from(first) != code)
                    .then(|| format!(":{}", u32::from(first)))
            })
            .unwrap_or_default()
    } else {
        String::new()
    };
    let text = if all && flags & 16 != 0 && !released && !m.control && !m.alt && printable {
        ks.key_char
            .as_deref()
            .map(|t| format!(";{}", codepoints(t)))
            .unwrap_or_default()
    } else {
        String::new()
    };
    Some(format!("\x1b[{code}{alternate};{modifier}{event_suffix}{text}u").into_bytes())
}

/// Option을 Meta로(OWE-658): 켜져 있으면 Option+글자를 ESC+글자로 보낸다. [`key`]가 `None`을 돌려준 키에만 쓴다
/// (레거시 모드의 Option+글자. kitty 모드는 [`key`]가 이미 Alt를 CSI-u에 싣는다). 끄면 IME가 특수 문자를 넣는다.
// ponytail: Shift는 영문자만 대문자로 바꾼다(Option+Shift+1 → ESC 1). 기호까지 필요하면 자판 배열 조회를 붙인다.
pub fn meta(ks: &Keystroke, option_as_meta: bool) -> Option<Vec<u8>> {
    let m = ks.modifiers;
    if !option_as_meta || !m.alt || m.control || m.platform || m.function {
        return None;
    }
    let mut chars = ks.key.chars();
    let c = chars
        .next()
        .filter(|c| c.is_ascii_graphic() && chars.next().is_none())?;
    let c = if m.shift { c.to_ascii_uppercase() } else { c };
    Some(vec![0x1b, u8::try_from(c).ok()?])
}

fn codepoints(text: &str) -> String {
    text.chars()
        .map(|c| u32::from(c).to_string())
        .collect::<Vec<_>>()
        .join(":")
}

/// IME 확정 텍스트. report-all일 때만 CSI-u로 바꾸며 preedit는 이 함수에 오지 않는다.
pub fn text(text: &[u8], modes: Modes) -> Vec<u8> {
    if modes.kitty & 8 == 0 {
        return text.to_vec();
    }
    let Ok(text) = std::str::from_utf8(text) else {
        return text.to_vec();
    };
    text.chars()
        .flat_map(|c| {
            let n = u32::from(c);
            let associated = if modes.kitty & 16 != 0 {
                format!(";{n}")
            } else {
                String::new()
            };
            format!("\x1b[{n};1{associated}u").into_bytes()
        })
        .collect()
}

#[derive(Debug, Clone, Copy)]
pub enum Mouse {
    Press(u8),
    Release(u8),
    Move(Option<u8>),
    Wheel(bool),
}

/// SGR 1006. 칸은 0부터 받아 1부터 출력한다. Shift는 로컬 상호작용을 위해 보고를 우회한다.
pub fn mouse(modes: Modes, event: Mouse, column: u16, line: u16, m: Modifiers) -> Option<Vec<u8>> {
    if !modes.mouse() || !modes.contains(Modes::SGR_MOUSE) || m.shift || m.platform {
        return None;
    }
    let (button, release) = match event {
        Mouse::Press(b) => (b, false),
        Mouse::Release(b) => (b, true),
        Mouse::Wheel(up) => (if up { 64 } else { 65 }, false),
        Mouse::Move(b) => {
            if !(modes.contains(Modes::MOUSE_MOTION)
                || modes.contains(Modes::MOUSE_DRAG) && b.is_some())
            {
                return None;
            }
            (32 + b.unwrap_or(3), false)
        }
    };
    let button = button + 8 * u8::from(m.alt) + 16 * u8::from(m.control);
    Some(
        format!(
            "\x1b[<{button};{};{}{}",
            u32::from(column) + 1,
            u32::from(line) + 1,
            if release { 'm' } else { 'M' }
        )
        .into_bytes(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn encode(k: &str, flags: u8, event: Event) -> Option<Vec<u8>> {
        key(
            &Keystroke::parse(k).unwrap(),
            Modes {
                kitty: flags,
                flags: 0,
            },
            event,
        )
    }

    /// OWE-658: Option+b는 켜면 ESC b, 끄면 IME(특수 문자)로 간다.
    #[test]
    fn option_as_meta_sends_escape_prefix() {
        let ks = |k: &str| Keystroke::parse(k).unwrap();
        // 레거시 모드에서 `key`는 Option+글자를 보내지 않아 `meta`로 넘어간다.
        assert_eq!(encode("alt-b", 0, Event::Press), None);
        assert_eq!(meta(&ks("alt-b"), true).unwrap(), b"\x1bb");
        assert_eq!(meta(&ks("alt-b"), false), None);
        assert_eq!(meta(&ks("alt-shift-f"), true).unwrap(), b"\x1bF");
        assert_eq!(meta(&ks("alt-."), true).unwrap(), b"\x1b.");
        // Option 없는 글자·Ctrl/Cmd 조합·이름 있는 키는 대상이 아니다.
        assert_eq!(meta(&ks("b"), true), None);
        assert_eq!(meta(&ks("ctrl-alt-b"), true), None);
        assert_eq!(meta(&ks("cmd-alt-b"), true), None);
        assert_eq!(meta(&ks("alt-left"), true), None);
    }

    /// AS-03: F1–F4는 레거시 SS3(`ESC O P..S`), 수식키·이벤트 보고는 `CSI 1;m` 계열.
    /// kitty 협상 때도 비표준 `CSI 5736xu`를 보내지 않는다.
    #[test]
    fn f1_to_f4_use_legacy_family_in_all_modes() {
        // 레거시(플래그 0): 기존에는 `None`이라 htop·mc·vim에서 동작하지 않았다.
        for (key, letter) in [("f1", "P"), ("f2", "Q"), ("f3", "R"), ("f4", "S")] {
            let want = format!("\x1bO{letter}").into_bytes();
            assert_eq!(encode(key, 0, Event::Press).unwrap(), want, "{key}");
            assert_eq!(encode(key, 0, Event::Release), None, "{key} release");
        }
        // 레거시 수식키.
        assert_eq!(encode("ctrl-f1", 0, Event::Press).unwrap(), b"\x1b[1;5P");
        assert_eq!(encode("shift-f3", 0, Event::Press).unwrap(), b"\x1b[1;2R");
        // kitty 협상 때도 SS3 계열(수식키 있으면 `CSI 1;m`).
        for flags in [1, 5, 7, 31] {
            assert_eq!(
                encode("f1", flags, Event::Press).unwrap(),
                b"\x1bOP",
                "flags={flags}"
            );
            assert_eq!(
                encode("shift-f2", flags, Event::Press).unwrap(),
                b"\x1b[1;2Q",
                "flags={flags}"
            );
        }
        // 이벤트 보고 모드의 Release.
        assert_eq!(encode("f4", 7, Event::Release).unwrap(), b"\x1b[1;1:3S");
    }

    #[test]
    fn agent_shift_enter_and_recovery_keys() {
        for flags in [5, 7] {
            assert_eq!(
                encode("shift-enter", flags, Event::Press).unwrap(),
                b"\x1b[13;2u"
            );
            assert_eq!(encode("enter", flags, Event::Press).unwrap(), b"\r");
            assert_eq!(encode("enter", flags, Event::Release), None);
            assert_eq!(
                encode("ctrl-c", flags, Event::Press).unwrap(),
                b"\x1b[99;5u"
            );
            assert_eq!(encode("a", flags, Event::Press), None);
        }
        assert_eq!(encode("left", 7, Event::Release).unwrap(), b"\x1b[1;1:3D");
        assert_eq!(
            encode("shift-enter", 7, Event::Repeat).unwrap(),
            b"\x1b[13;2:2u"
        );
        assert_eq!(encode("left", 5, Event::Release), None);
        assert_eq!(encode("cmd-t", 31, Event::Press), None);
        // Cmd+Backspace = Ctrl+U(줄 시작까지 지움), 협상한 형식 그대로.
        assert_eq!(encode("cmd-backspace", 0, Event::Press).unwrap(), b"\x15");
        assert_eq!(
            encode("cmd-backspace", 1, Event::Press).unwrap(),
            b"\x1b[117;5u"
        );
        assert_eq!(encode("cmd-backspace", 0, Event::Release), None);
        assert_eq!(encode("cmd-shift-backspace", 0, Event::Press), None);
        // Cmd+←/→ = Ctrl+A/Ctrl+E(줄 시작·끝).
        assert_eq!(encode("cmd-left", 0, Event::Press).unwrap(), b"\x01");
        assert_eq!(encode("cmd-right", 0, Event::Press).unwrap(), b"\x05");
        assert_eq!(encode("cmd-shift-left", 0, Event::Press), None);
        for flags in [0, 5, 7, 31] {
            for ev in [Event::Press, Event::Repeat, Event::Release] {
                for shortcut in [
                    "ctrl-tab",
                    "ctrl-shift-tab",
                    "ctrl-pageup",
                    "ctrl-pagedown",
                    "ctrl-1",
                    "ctrl-2",
                    "ctrl-9",
                ] {
                    assert_eq!(
                        encode(shortcut, flags, ev),
                        None,
                        "{shortcut} flags={flags}"
                    );
                }
            }
        }
    }

    #[test]
    fn ime_commits_all_keys_and_associated_text() {
        assert_eq!(
            text("한글".as_bytes(), Modes { kitty: 7, flags: 0 }),
            "한글".as_bytes()
        );
        assert_eq!(
            text(
                "한".as_bytes(),
                Modes {
                    kitty: 24,
                    flags: 0
                }
            ),
            b"\x1b[54620;1;54620u"
        );
        assert_eq!(
            encode("enter", 11, Event::Release).unwrap(),
            b"\x1b[13;1:3u"
        );
    }

    #[test]
    fn sgr_mouse_click_release_drag_wheel_and_bypass() {
        let modes = Modes {
            kitty: 0,
            flags: Modes::MOUSE_DRAG | Modes::SGR_MOUSE,
        };
        let m = Modifiers::default();
        assert_eq!(
            mouse(modes, Mouse::Press(0), 299, 9, m).unwrap(),
            b"\x1b[<0;300;10M"
        );
        assert_eq!(
            mouse(modes, Mouse::Release(0), 299, 9, m).unwrap(),
            b"\x1b[<0;300;10m"
        );
        assert_eq!(
            mouse(modes, Mouse::Move(Some(0)), 2, 3, m).unwrap(),
            b"\x1b[<32;3;4M"
        );
        assert_eq!(
            mouse(modes, Mouse::Wheel(true), 2, 3, m).unwrap(),
            b"\x1b[<64;3;4M"
        );
        assert!(mouse(modes, Mouse::Move(None), 0, 0, m).is_none());
        assert!(mouse(modes, Mouse::Press(0), 0, 0, Modifiers { shift: true, ..m }).is_none());
        assert!(mouse(Modes::default(), Mouse::Press(0), 0, 0, m).is_none());
    }
}
