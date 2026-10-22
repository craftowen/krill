use std::fs;

use gpui::{Entity, TestAppContext, VisualTestContext};
use krill_proto::screen::{Cell, Flags, Link, Op, Revision, Seq, encode_update};

use super::*;
use crate::shell::tests::{layout_of, open, state_path};

struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> Self {
        let path =
            std::env::temp_dir().join(format!("krill-terminal-menu-{}-{tag}", std::process::id()));
        fs::create_dir_all(&path).unwrap();
        Self(path.canonicalize().unwrap())
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).ok();
    }
}

fn show_output(
    view: &Entity<Shell>,
    cwd: &std::path::Path,
    text: &str,
    uri: Option<&str>,
    cx: &mut VisualTestContext,
) -> (PaneId, Point<Pixels>) {
    let pane = layout_of(view, cx).active_tab().focused;
    let mut cells = Vec::new();
    for ch in text.chars() {
        let wide = !ch.is_ascii();
        cells.push(Cell {
            ch,
            flags: if wide { Flags::WIDE } else { Flags::default() },
            ..Cell::default()
        });
        if wide {
            cells.push(Cell {
                flags: Flags::SPACER,
                ..Cell::default()
            });
        }
    }
    let cols = u16::try_from(cells.len()).unwrap();
    let (ours, _theirs) = UnixStream::pair().unwrap();
    view.update(cx, |s, cx| {
        s.layout.set_launch(
            pane,
            Create {
                account: None,
                program: Program::Login,
                cwd: cwd.as_os_str().as_bytes().to_vec(),
                cols: 80,
                rows: 24,
            },
        );
        s.attach_test(pane, krill_proto::SessionId(22), ours, cx);
        let mut ops = vec![
            Op::Reset {
                cols: 100,
                lines: 4,
            },
            Op::Line { line: 1, cells },
        ];
        if let Some(uri) = uri {
            ops.push(Op::Links {
                line: 1,
                links: vec![Link {
                    column: 0,
                    cols,
                    uri: uri.into(),
                }],
            });
        }
        let frame = &encode_update(Revision(1), Seq(0), ops).unwrap()[0];
        let (kind, body) = ipc::read_frame(&mut &frame[..]).unwrap().unwrap();
        assert!(s.on_frame(pane, kind, &body, cx));
    });
    cx.run_until_parked();
    let (bounds, cell) = cx.update(|_, cx| view.read(cx).live[&pane].painted.get());
    (
        pane,
        point(
            bounds.origin.x + cell.width * 4.5,
            bounds.origin.y + cell.height * 1.5,
        ),
    )
}

fn right_click(at: Point<Pixels>, cx: &mut VisualTestContext) {
    cx.simulate_mouse_move(at, None, Modifiers::default());
    cx.simulate_mouse_down(at, MouseButton::Right, Modifiers::default());
    cx.simulate_mouse_up(at, MouseButton::Right, Modifiers::default());
    cx.run_until_parked();
}

fn click(selector: &'static str, cx: &mut VisualTestContext) {
    let bounds = cx
        .debug_bounds(selector)
        .unwrap_or_else(|| panic!("missing {selector}"));
    cx.simulate_click(bounds.center(), Modifiers::default());
    cx.run_until_parked();
}

#[gpui::test]
fn terminal_file_menu_copies_full_path_and_opens_at_line(cx: &mut TestAppContext) {
    let dir = TempDir::new("plain");
    fs::create_dir_all(dir.0.join("src")).unwrap();
    let file = dir.0.join("src/my file.rs");
    fs::write(&file, "first\nsecond\n").unwrap();
    let (view, cx) = open(cx, state_path("terminal-menu-plain"));
    let (_, at) = show_output(&view, &dir.0, "src/my file.rs:2:3", None, cx);
    right_click(at, cx);
    for selector in [
        "terminal-file-open",
        "terminal-file-open-default",
        "terminal-file-reveal",
        "terminal-file-copy-path",
    ] {
        assert!(cx.debug_bounds(selector).is_some(), "{selector}");
    }
    click("terminal-file-copy-path", cx);
    assert_eq!(
        cx.read_from_clipboard().unwrap().text().as_deref(),
        Some(file.to_str().unwrap())
    );
    right_click(at, cx);
    click("terminal-file-open", cx);
    let layout = layout_of(&view, cx);
    let pane = layout.active_tab().focused;
    assert_eq!(layout.file(pane), Some(file.as_path()));
    cx.simulate_keystrokes("shift-right cmd-c");
    assert_eq!(
        cx.read_from_clipboard().unwrap().text().as_deref(),
        Some("c"),
        "2번째 줄 3번째 글자에서 열어야 한다"
    );
}

#[gpui::test]
fn terminal_osc8_file_menu_uses_uri_with_korean_and_spaces(cx: &mut TestAppContext) {
    let dir = TempDir::new("osc8");
    let file = dir.0.join("보정 완료.jpeg");
    fs::write(&file, "image").unwrap();
    let uri = gpui::http_client::Url::from_file_path(&file)
        .unwrap()
        .to_string();
    let (view, cx) = open(cx, state_path("terminal-menu-osc8"));
    let (_, at) = show_output(&view, &dir.0, "다운로드 링크", Some(&uri), cx);
    right_click(at, cx);
    click("terminal-file-copy-path", cx);
    assert_eq!(
        cx.read_from_clipboard().unwrap().text().as_deref(),
        Some(file.to_str().unwrap())
    );
}

#[gpui::test]
fn terminal_file_menu_ignores_missing_web_and_remote_targets(cx: &mut TestAppContext) {
    let dir = TempDir::new("not-local");
    let file = dir.0.join("exists.rs");
    fs::write(&file, "code").unwrap();
    let (view, cx) = open(cx, state_path("terminal-menu-not-local"));
    for uri in [
        None,
        Some("https://example.com/exists.rs"),
        Some("file://other-host/etc/hosts"),
    ] {
        let (_, at) = show_output(&view, &dir.0, "missing.rs", uri, cx);
        right_click(at, cx);
        assert!(cx.debug_bounds("terminal-file-open").is_none());
        cx.simulate_keystrokes("escape");
    }
    let (pane, at) = show_output(&view, &dir.0, "exists.rs", None, cx);
    view.update(cx, |s, _| {
        s.layout
            .set_host(pane, krill_app::hosts::HostId::Remote("other-host".into()));
    });
    right_click(at, cx);
    assert!(
        cx.debug_bounds("terminal-file-open").is_none(),
        "원격 경로를 로컬 파일로 열지 않는다"
    );
}
