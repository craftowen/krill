//! MK-045: bounded CLI screen capture. The client never builds a terminal engine.

use std::fmt::Write as _;
use std::io;

use krill_proto::control::{CaptureFormat, MAX_CAPTURE};
use krill_proto::screen::{Cell, Color, CursorShape, Flags, Screen};

pub(crate) fn capture(
    s: &Screen,
    modes: &str,
    format: CaptureFormat,
    active: &Cell,
) -> io::Result<Vec<u8>> {
    let mut out = String::new();
    if format == CaptureFormat::Ansi {
        out.push_str("\x1b[?6l\x1b[r\x1b[4l\x1b[?7l\x1b[0m\x1b[2J\x1b[H");
    }
    let mut previous: Option<&Cell> = None;
    for (line, row) in s.rows.iter().enumerate() {
        if format == CaptureFormat::Ansi {
            write!(out, "\x1b[{};1H", line + 1).ok();
        }
        for cell in row {
            if cell.flags.contains(Flags::SPACER) {
                continue;
            }
            if format == CaptureFormat::Ansi && previous.is_none_or(|p| !same_style(p, cell)) {
                style(&mut out, cell);
                previous = Some(cell);
            }
            // Engine cells are printable, but keep capture from emitting controls from malformed state.
            out.push(if cell.ch.is_control() { ' ' } else { cell.ch });
            out.extend(cell.zerowidth.iter().copied().filter(|c| !c.is_control()));
            bounded(&out)?;
        }
        if format == CaptureFormat::Plain {
            while out.ends_with(' ') {
                out.pop();
            }
            out.push('\n');
        }
    }
    match format {
        CaptureFormat::Plain => {
            while out.ends_with('\n') {
                out.pop();
            }
            out.push('\n');
        }
        CaptureFormat::Ansi => {
            style(&mut out, active);
            out.push_str(modes);
            let shape = if s.cursor_explicit {
                (match s.cursor_style.shape {
                    CursorShape::Block => 1,
                    CursorShape::Underline => 3,
                    CursorShape::Bar => 5,
                }) + u8::from(!s.cursor_style.blink)
            } else {
                0
            };
            write!(
                out,
                "\x1b[{shape} q\x1b[{};{}H",
                s.cursor.line + 1,
                s.cursor.column + 1
            )
            .ok();
        }
    }
    bounded(&out)?;
    Ok(out.into_bytes())
}

fn bounded(out: &str) -> io::Result<()> {
    if out.len() > MAX_CAPTURE {
        Err(io::Error::other("screen capture exceeds byte limit"))
    } else {
        Ok(())
    }
}

fn same_style(a: &Cell, b: &Cell) -> bool {
    a.fg == b.fg && a.bg == b.bg && a.underline == b.underline && a.flags == b.flags
}

fn style(out: &mut String, cell: &Cell) {
    out.push_str("\x1b[0");
    for (flag, code) in [
        (Flags::BOLD, 1),
        (Flags::DIM, 2),
        (Flags::ITALIC, 3),
        (Flags::UNDERLINE, 4),
        (Flags::INVERSE, 7),
        (Flags::HIDDEN, 8),
        (Flags::STRIKEOUT, 9),
    ] {
        if cell.flags.contains(flag) {
            write!(out, ";{code}").ok();
        }
    }
    for (value, code) in [(cell.fg, 38), (cell.bg, 48), (cell.underline, 58)] {
        match value {
            Color::Default => {}
            Color::Indexed(i) => {
                write!(out, ";{code};5;{i}").ok();
            }
            Color::Rgb(c) => {
                write!(out, ";{code};2;{};{};{}", c.r, c.g, c.b).ok();
            }
        }
    }
    out.push('m');
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        Appearance,
        term::{Engine, Feed},
    };
    use krill_proto::holder::WinSize;

    #[test]
    fn capture_restores_unicode_colors_cursor_and_input_modes() {
        let size = WinSize { cols: 80, rows: 24 };
        let mut original = Engine::new(size);
        let look = Appearance::load(std::path::PathBuf::new());
        original.feed(
            "\x1b[31m한글e\u{301}\x1b[0m\r\nnext\x1b[?1h\x1b[?2004h".as_bytes(),
            Feed::Live,
            &look,
        );
        let bytes = original.capture_cli(CaptureFormat::Ansi).unwrap();
        let mut restored = Engine::new(size);
        assert!(restored.feed(&bytes, Feed::Replay, &look).is_empty());
        let before = original.screen();
        let after = restored.screen();
        assert_eq!(before.rows, after.rows);
        assert_eq!(before.cursor, after.cursor);
        assert_eq!(
            original.capture_cli(CaptureFormat::Plain).unwrap(),
            "한글e\u{301}\nnext\n".as_bytes()
        );
        assert!(bytes.windows(5).any(|b| b == b"\x1b[?1h"));
    }
    proptest::proptest! {
        #[test]
        fn arbitrary_boundary_input_does_not_panic(bytes in proptest::collection::vec(proptest::prelude::any::<u8>(), 0..20000)) {
            let mut boundary = Boundary::default();
            for chunk in bytes.chunks(31) { boundary.advance(chunk); }
            proptest::prop_assert!(boundary.pending <= bytes.len());
        }
    }

    #[test]
    fn ascii_chunks_preserve_pending_escape_strings_and_utf8() {
        for prefix in [
            b"".as_slice(),
            b"\x1b",
            b"\x1b[31",
            b"\x1b]2;title",
            b"\x1bPpayload",
            b"\xe9\x9f",
        ] {
            let mut chunked = Boundary::default();
            let mut bytewise = Boundary::default();
            chunked.advance(prefix);
            bytewise.advance(prefix);
            for chunk in [b"plain ascii\r\n".as_slice(), b"m\x07\x1b\\", b"after"] {
                chunked.advance(chunk);
                for byte in chunk {
                    bytewise.advance(std::slice::from_ref(byte));
                }
                assert_eq!(chunked.pending, bytewise.pending, "prefix={prefix:?}");
                assert_eq!(chunked.escaped, bytewise.escaped, "prefix={prefix:?}");
                assert_eq!(
                    std::mem::discriminant(&chunked.state),
                    std::mem::discriminant(&bytewise.state),
                    "prefix={prefix:?}"
                );
            }
        }
    }

    #[test]
    fn capture_continues_pending_sequences_sync_and_active_sgr() {
        let size = WinSize { cols: 80, rows: 24 };
        let look = Appearance::load(std::path::PathBuf::new());
        let cases: &[(&[u8], &[u8])] = &[
            (b"before \x1b[31", b"mred"),
            (b"before \xe9\x9f", b"\x93"),
            (b"before \x1b]2;part", b"ial\x07after"),
            (b"before \x1b]2;cut\x1b[31mred", b"more"),
            (b"before \x1bXsos\x1b[31mred", b"more"),
            (b"before \x1b[?2026hbuffer", b"ed\x1b[?2026l"),
            (b"\x1b[31mred", b"more"),
        ];
        for &(prefix, suffix) in cases {
            let mut original = Engine::new(size);
            original.feed(prefix, Feed::Live, &look);
            let snapshot = original.capture_cli(CaptureFormat::Ansi).unwrap();
            let rewind = original.capture_rewind();
            let mut restored = Engine::new(size);
            restored.feed(&snapshot, Feed::Replay, &look);
            restored.feed(&prefix[prefix.len() - rewind..], Feed::Replay, &look);
            restored.feed(suffix, Feed::Replay, &look);
            original.feed(suffix, Feed::Live, &look);
            assert_eq!(
                original.screen().rows,
                restored.screen().rows,
                "prefix={prefix:?}"
            );
            assert_eq!(original.screen().cursor, restored.screen().cursor);
        }
    }
}

/// Incomplete UTF-8/escape bytes have not affected the captured screen yet.
/// Rewind the raw subscription over them rather than sending a continuation to a fresh host parser.
#[derive(Default)]
pub(crate) struct Boundary {
    state: BoundaryState,
    pub pending: usize,
    escaped: bool,
}
#[derive(Default)]
enum BoundaryState {
    #[default]
    Ground,
    Escape,
    Csi,
    String {
        osc: bool,
    },
    Utf8(u8),
}
impl Boundary {
    pub fn advance(&mut self, bytes: &[u8]) {
        // ASCII without ESC cannot leave ground or start a partial UTF-8 sequence.
        // Keep pending control strings and codepoints on the bytewise path.
        if matches!(self.state, BoundaryState::Ground) && bytes.is_ascii() && !bytes.contains(&0x1b)
        {
            return;
        }
        for &byte in bytes {
            self.pending = self.pending.saturating_add(1);
            match self.state {
                BoundaryState::Ground => self.start(byte),
                BoundaryState::Utf8(left) => {
                    if byte & 0xc0 != 0x80 {
                        self.start(byte);
                    } else if left == 1 {
                        self.clear();
                    } else {
                        self.state = BoundaryState::Utf8(left - 1);
                    }
                }
                BoundaryState::Escape => match byte {
                    b'[' => self.state = BoundaryState::Csi,
                    b']' | b'P' | b'_' | b'^' => {
                        self.state = BoundaryState::String { osc: byte == b']' }
                    }
                    0x1b => self.start(byte),
                    0x30..=0x7e | 0x18 | 0x1a => self.clear(),
                    _ => {}
                },
                BoundaryState::Csi => match byte {
                    0x40..=0x7e | 0x18 | 0x1a => self.clear(),
                    0x1b => self.start(byte),
                    _ => {}
                },
                BoundaryState::String { osc } => {
                    if (osc && byte == 7)
                        || (self.escaped && byte == b'\\')
                        || byte == 0x18
                        || byte == 0x1a
                    {
                        self.clear();
                    }
                    self.escaped = byte == 0x1b;
                }
            }
        }
    }
    fn clear(&mut self) {
        self.pending = 0;
        self.state = BoundaryState::Ground;
        self.escaped = false;
    }
    fn start(&mut self, byte: u8) {
        self.pending = 1;
        self.state = match byte {
            0x1b => BoundaryState::Escape,
            0xc2..=0xdf => BoundaryState::Utf8(1),
            0xe0..=0xef => BoundaryState::Utf8(2),
            0xf0..=0xf4 => BoundaryState::Utf8(3),
            _ => {
                self.clear();
                return;
            }
        };
    }
}
