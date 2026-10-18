use std::fs;

use gpui::{Entity, Focusable as _, TestAppContext, VisualTestContext};

use super::*;
use crate::shell::tests::{layout_of, open, state_path};

struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("mk-files-{}-{tag}", std::process::id()));
        fs::remove_dir_all(&dir).ok();
        fs::create_dir_all(&dir).unwrap();
        Self(dir.canonicalize().unwrap())
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).ok();
    }
}

fn open_file(view: &Entity<Shell>, path: &Path, cx: &mut VisualTestContext) -> PaneId {
    let path = path.to_owned();
    view.update(cx, |s, cx| s.open_file(path, cx));
    cx.run_until_parked();
    layout_of(view, cx).active_tab().focused
}

fn editor_text(view: &Entity<Shell>, pane: PaneId, cx: &mut VisualTestContext) -> String {
    cx.update(|_, cx| {
        let shell = view.read(cx);
        let state = shell.files.tabs[&pane].editor().unwrap().clone();
        state.read(cx).text().to_string()
    })
}

fn dirty(view: &Entity<Shell>, pane: PaneId, cx: &mut VisualTestContext) -> bool {
    cx.update(|_, cx| view.read(cx).files.tabs[&pane].is_dirty())
}

/// 디스크 감시 한 주기를 돌린다.
fn tick(cx: &mut VisualTestContext) {
    cx.executor().advance_clock(WATCH_EVERY * 2);
    cx.run_until_parked();
}

/// 열기 → 키 입력은 에디터로(터미널 아님) → dirty 점 → ⌘S 원자적 저장 → 깨끗.
#[gpui::test]
fn open_type_dirty_and_save(cx: &mut TestAppContext) {
    let dir = TempDir::new("save");
    let file = dir.0.join("main.rs");
    fs::write(&file, "fn main() {}\n").unwrap();
    let (view, cx) = open(cx, state_path("files-save"));
    let pane = open_file(&view, &file, cx);

    let l = layout_of(&view, cx);
    assert_eq!(l.tabs().len(), 2);
    assert_eq!(l.file(pane), Some(file.as_path()));
    assert_eq!(l.session(pane), None);
    assert!(cx.debug_bounds("file-pane").is_some());
    let label = cx.update(|_, cx| view.read(cx).pane_label(pane, cx));
    assert_eq!(label, "main.rs");
    let focused = cx.update(|window, cx| {
        let shell = view.read(cx);
        let editor = shell.files.tabs[&pane].editor().unwrap().read(cx);
        (
            editor.focus_handle(cx).is_focused(window),
            shell.focus.is_focused(window),
        )
    });
    assert_eq!(
        focused,
        (true, false),
        "keys go to the editor, not the terminal"
    );

    cx.simulate_input("x");
    assert_eq!(editor_text(&view, pane, cx), "xfn main() {}\n");
    assert!(dirty(&view, pane, cx));
    assert!(cx.debug_bounds("file-dirty").is_some());

    cx.simulate_keystrokes("cmd-s");
    assert_eq!(fs::read_to_string(&file).unwrap(), "xfn main() {}\n");
    assert!(!dirty(&view, pane, cx));
    assert!(cx.debug_bounds("file-dirty").is_none());
    // 자기 저장은 외부 변경으로 보지 않는다.
    tick(cx);
    assert!(cx.debug_bounds("file-changed-on-disk").is_none());

    // 같은 파일을 다시 열면 새 탭이 아니라 그 탭으로 간다.
    cx.simulate_keystrokes("cmd-1");
    assert_eq!(open_file(&view, &file, cx), pane);
    assert_eq!(layout_of(&view, cx).tabs().len(), 2);
}

/// 깨끗한 탭은 외부 변경을 자동 재로드, dirty 탭은 표시만 하고 편집을 지킨다. 삭제는 표시한다.
#[gpui::test]
fn external_change_reload_mark_and_delete(cx: &mut TestAppContext) {
    let dir = TempDir::new("external");
    let file = dir.0.join("notes.txt");
    fs::write(&file, "one\n").unwrap();
    let (view, cx) = open(cx, state_path("files-external"));
    let pane = open_file(&view, &file, cx);

    fs::write(&file, "two lines\nhere\n").unwrap();
    tick(cx);
    assert_eq!(editor_text(&view, pane, cx), "two lines\nhere\n");
    assert!(!dirty(&view, pane, cx));

    cx.simulate_input("!");
    fs::write(&file, "outside edit, longer\n").unwrap();
    tick(cx);
    assert_eq!(
        editor_text(&view, pane, cx),
        "!two lines\nhere\n",
        "draft kept"
    );
    assert!(cx.debug_bounds("file-changed-on-disk").is_some());

    // '디스크 내용으로 다시 읽기'.
    cx.update(|window, cx| view.update(cx, |s, cx| s.reload_file(pane, window, cx)));
    cx.run_until_parked();
    assert_eq!(editor_text(&view, pane, cx), "outside edit, longer\n");
    assert!(cx.debug_bounds("file-changed-on-disk").is_none());

    fs::remove_file(&file).unwrap();
    tick(cx);
    assert!(cx.debug_bounds("file-deleted").is_some());
}

/// 미저장 탭 닫기는 묻는다: 취소하면 남고, 저장 안 함이면 닫힌다. 저장하면 디스크에 쓰고 닫힌다.
#[gpui::test]
fn closing_dirty_tab_asks(cx: &mut TestAppContext) {
    let dir = TempDir::new("close");
    // 마크다운은 미리보기로 열려 입력이 에디터로 가지 않으므로 일반 텍스트로 시험한다.
    let file = dir.0.join("a.txt");
    fs::write(&file, "a\n").unwrap();
    let (view, cx) = open(cx, state_path("files-close"));
    let pane = open_file(&view, &file, cx);
    cx.simulate_input("z");

    cx.simulate_keystrokes("cmd-w");
    assert!(cx.has_pending_prompt());
    cx.simulate_prompt_answer(etext::CANCEL.get());
    cx.run_until_parked();
    assert_eq!(layout_of(&view, cx).file(pane), Some(file.as_path()));
    assert!(dirty(&view, pane, cx));

    cx.simulate_keystrokes("cmd-w");
    cx.simulate_prompt_answer(etext::SAVE.get());
    cx.run_until_parked();
    assert_eq!(layout_of(&view, cx).file(pane), None);
    assert_eq!(fs::read_to_string(&file).unwrap(), "za\n");

    let pane = open_file(&view, &file, cx);
    cx.simulate_input("y");
    cx.simulate_keystrokes("cmd-shift-w");
    cx.simulate_prompt_answer(etext::DISCARD.get());
    cx.run_until_parked();
    assert_eq!(layout_of(&view, cx).file(pane), None);
    assert_eq!(fs::read_to_string(&file).unwrap(), "za\n");
    assert!(cx.update(|_, cx| view.read(cx).files.tabs.is_empty()));

    // 깨끗한 탭은 묻지 않고 닫힌다.
    let pane = open_file(&view, &file, cx);
    cx.simulate_keystrokes("cmd-w");
    assert!(!cx.has_pending_prompt());
    assert_eq!(layout_of(&view, cx).file(pane), None);
}

/// 감사 2026-10-17: 감시가 돌기 전 바깥에서 바뀐 파일을 ⌘S가 덮어쓰지 않는다(lost update). 오류를 띄우고 편집은 지킨다.
#[gpui::test]
fn save_does_not_clobber_external_change(cx: &mut TestAppContext) {
    let dir = TempDir::new("clobber");
    let file = dir.0.join("a.txt");
    fs::write(&file, "a\n").unwrap();
    let (view, cx) = open(cx, state_path("files-clobber"));
    let pane = open_file(&view, &file, cx);
    cx.simulate_input("z");
    fs::write(&file, "agent edit\n").unwrap();

    cx.simulate_keystrokes("cmd-s");
    assert_eq!(fs::read_to_string(&file).unwrap(), "agent edit\n");
    assert!(dirty(&view, pane, cx));
    assert!(cx.debug_bounds("file-error").is_some());

    // '내 편집 유지'를 고르면 저장한다.
    tick(cx);
    assert!(cx.debug_bounds("file-changed-on-disk").is_some());
    view.update(cx, |s, _| {
        let doc = s.files.tabs.get_mut(&pane).unwrap().doc.as_mut().unwrap();
        doc.keep_mine(editor::stamp(&file).unwrap());
    });
    cx.simulate_keystrokes("cmd-s");
    assert_eq!(fs::read_to_string(&file).unwrap(), "za\n");
    assert!(!dirty(&view, pane, cx));
}

/// 감사 2026-10-17: 메뉴바 아이콘 '종료'·창 밖 ⌘Q(전역 핸들러)는 `cx.quit()`로 미저장 확인을 건너뛰었다.
/// 모든 종료 경로가 `quit_app`으로 모여 묻는다. 메뉴바가 이름으로 만드는 액션도 실제로 있다.
#[gpui::test]
fn every_quit_path_asks_about_unsaved_files(cx: &mut TestAppContext) {
    let dir = TempDir::new("quit");
    let file = dir.0.join("a.txt");
    fs::write(&file, "a\n").unwrap();
    let (view, cx) = open(cx, state_path("files-quit"));
    let pane = open_file(&view, &file, cx);
    cx.simulate_input("z");

    assert!(cx.update(|_, cx| cx.build_action("shell::Quit", None).is_ok()));
    cx.update(|_, cx| quit_app(cx));
    cx.run_until_parked();
    assert!(cx.has_pending_prompt());
    cx.simulate_prompt_answer(etext::CANCEL.get());
    cx.run_until_parked();
    assert!(dirty(&view, pane, cx));

    // 셸 창 안 ⌘Q(액션 처리 중 창이 빌려진 상태)도 미저장 창을 놓치지 않는다.
    cx.simulate_keystrokes("cmd-q");
    cx.run_until_parked();
    assert!(cx.has_pending_prompt());
    cx.simulate_prompt_answer(etext::CANCEL.get());
    cx.run_until_parked();
    assert!(dirty(&view, pane, cx));
    assert_eq!(fs::read_to_string(&file).unwrap(), "a\n");
}

/// 터미널 탭으로 돌아오면 키보드 포커스도 셸(터미널)로 돌아온다. 재시작하면 파일 탭 경로가 복원된다.
#[gpui::test]
fn focus_returns_to_terminal_and_tabs_restore(cx: &mut TestAppContext) {
    let dir = TempDir::new("restore");
    let file = dir.0.join("b.json");
    fs::write(&file, "{}\n").unwrap();
    let path = state_path("files-restore");
    let (view, vcx) = open(cx, path.clone());
    let pane = open_file(&view, &file, vcx);
    vcx.simulate_keystrokes("cmd-1");
    vcx.run_until_parked();
    let on_shell = vcx.update(|window, cx| view.read(cx).focus.is_focused(window));
    assert!(on_shell);

    vcx.update(|_, cx| view.read(cx).flush_layout());
    let restored = Layout::load(&path).unwrap();
    assert_eq!(restored.file(pane), Some(file.as_path()));
}

#[gpui::test]
fn unopenable_file_shows_placeholder(cx: &mut TestAppContext) {
    let dir = TempDir::new("binary");
    let file = dir.0.join("blob.bin");
    fs::write(&file, b"\0\x01\x02").unwrap();
    let (view, cx) = open(cx, state_path("files-binary"));
    let pane = open_file(&view, &file, cx);
    let kind = cx.update(|_, cx| match view.read(cx).files.tabs[&pane].body {
        Body::Placeholder(kind, len) => Some((kind, len)),
        _ => None,
    });
    assert_eq!(kind, Some((Kind::Binary, 3)));
    // ⌘S는 아무것도 쓰지 않는다.
    cx.simulate_keystrokes("cmd-s");
    assert_eq!(fs::read(&file).unwrap(), b"\0\x01\x02");
}

/// 변경 패널 파일 행의 '에디터로 열기'는 가운데 영역에 파일 탭을 연다.
#[gpui::test]
fn diff_panel_open_file_event_opens_tab(cx: &mut TestAppContext) {
    let dir = TempDir::new("diff");
    let file = dir.0.join("changed.rs");
    fs::write(&file, "x\n").unwrap();
    let (view, cx) = open(cx, state_path("files-diff"));
    let target = file.clone();
    cx.update(|_, cx| {
        let panel = view.read(cx).diff.clone();
        panel.update(cx, |_, cx| cx.emit(krill_app::diff::OpenFile(target)));
    });
    cx.run_until_parked();
    let l = layout_of(&view, cx);
    assert_eq!(l.file(l.active_tab().focused), Some(file.as_path()));
}

/// ⌘P: 작업 폴더(.gitignore 존중) 퍼지 검색 → Enter로 연다.
#[gpui::test]
fn quick_open_finds_and_opens(cx: &mut TestAppContext) {
    let dir = TempDir::new("quick");
    fs::create_dir(dir.0.join("src")).unwrap();
    fs::write(dir.0.join("src/lib.rs"), "pub fn f() {}\n").unwrap();
    fs::write(dir.0.join("README.md"), "r\n").unwrap();
    let (view, cx) = open(cx, state_path("files-quick"));
    let root = dir.0.clone();
    view.update(cx, |s, cx| {
        s.new_folder_tab(&root, krill_proto::control::Program::Login, cx);
    });
    cx.run_until_parked();

    cx.simulate_keystrokes("cmd-p");
    cx.run_until_parked();
    assert!(cx.update(|_, cx| view.read(cx).files.quick_open.is_some()));
    cx.simulate_input("librs");
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    let l = layout_of(&view, cx);
    assert_eq!(
        l.file(l.active_tab().focused),
        Some(dir.0.join("src/lib.rs").as_path())
    );
    assert!(cx.update(|_, cx| view.read(cx).files.quick_open.is_none()));
}

fn previewing(view: &Entity<Shell>, pane: PaneId, cx: &mut VisualTestContext) -> Option<String> {
    cx.update(|_, cx| match &view.read(cx).files.tabs[&pane].body {
        Body::Text {
            shown: Some(Shown::Markdown(src, _)),
            ..
        } => Some(src.to_string()),
        Body::Text { shown: Some(_), .. } => Some(String::new()),
        _ => None,
    })
}

/// MK-121: 마크다운은 미리보기로 열리고(입력은 숨은 에디터로 가지 않는다), 편집으로 바꿔 고친 뒤
/// 미리보기로 돌아오면 고친 내용이 보인다. CSV는 표로 열린다. 이미지는 이미지 뷰, 외부 변경 시 다시 읽는다.
#[gpui::test]
fn markdown_preview_toggle_csv_table_and_image_reload(cx: &mut TestAppContext) {
    let dir = TempDir::new("viewers");
    let md = dir.0.join("README.md");
    fs::write(&md, "# hi\n").unwrap();
    let (view, cx) = open(cx, state_path("files-viewers"));
    let pane = open_file(&view, &md, cx);
    assert_eq!(previewing(&view, pane, cx).as_deref(), Some("# hi\n"));
    cx.simulate_keystrokes("x");
    assert_eq!(editor_text(&view, pane, cx), "# hi\n");

    cx.update(|window, cx| view.update(cx, |s, cx| s.set_preview(pane, false, window, cx)));
    cx.run_until_parked();
    assert_eq!(previewing(&view, pane, cx), None);
    cx.simulate_keystrokes("x");
    assert_eq!(editor_text(&view, pane, cx), "x# hi\n");
    cx.update(|window, cx| view.update(cx, |s, cx| s.set_preview(pane, true, window, cx)));
    cx.run_until_parked();
    assert_eq!(previewing(&view, pane, cx).as_deref(), Some("x# hi\n"));
    assert!(dirty(&view, pane, cx));

    let csv = dir.0.join("a.csv");
    fs::write(&csv, "a,b\n1,2\n").unwrap();
    let pane = open_file(&view, &csv, cx);
    let table = cx.update(|_, cx| {
        matches!(
            view.read(cx).files.tabs[&pane].body,
            Body::Text {
                shown: Some(Shown::Table(_)),
                ..
            }
        )
    });
    assert!(table);

    let png = dir.0.join("p.png");
    let header = |w: u8| {
        let mut b = b"\x89PNG\r\n\x1a\n\0\0\0\x0dIHDR".to_vec();
        b.extend_from_slice(&[0, 0, 0, w, 0, 0, 0, 1, 8, 6, 0, 0, 0]);
        b
    };
    fs::write(&png, header(1)).unwrap();
    let pane = open_file(&view, &png, cx);
    let image = |cx: &mut VisualTestContext| {
        cx.update(|_, cx| match &view.read(cx).files.tabs[&pane].body {
            Body::Image(v) => Some(v.entity_id()),
            _ => None,
        })
    };
    let before = image(cx).expect("image view");
    fs::write(&png, [header(2), vec![0]].concat()).unwrap();
    tick(cx);
    let after = image(cx).expect("image view after reload");
    assert_ne!(before, after, "외부 변경 뒤 이미지 뷰를 다시 만든다");
}

/// OWE-663 4번: 출력 속 `src/main.rs:3:4`(세션 cwd 기준 상대 경로)를 ⌘호버하면 밑줄, ⌘을 떼면 사라지고,
/// ⌘클릭하면 가운데 에디터 탭으로 열려 3번째 줄 4번째 글자에 커서가 간다.
#[gpui::test]
fn cmd_click_terminal_path_opens_editor_at_line(cx: &mut TestAppContext) {
    use gpui::{Modifiers, MouseButton, point};
    use krill_proto::screen::{Cell, Op, Revision, Seq, encode_update};

    let dir = TempDir::new("path-link");
    fs::create_dir_all(dir.0.join("src")).unwrap();
    let file = dir.0.join("src/main.rs");
    fs::write(&file, "a\nb\nfn main() {}\n").unwrap();
    let (view, cx) = open(cx, state_path("files-path-link"));
    let (ours, _theirs) = UnixStream::pair().unwrap();
    let pane = layout_of(&view, cx).active_tab().focused;
    view.update(cx, |s, cx| {
        s.layout.set_launch(
            pane,
            Create {
                account: None,
                program: Program::Login,
                cwd: dir.0.as_os_str().as_bytes().to_vec(),
                cols: 80,
                rows: 24,
            },
        );
        s.attach_test(pane, krill_proto::SessionId(12), ours, cx);
        let row = |t: &str| {
            t.chars()
                .map(|ch| Cell {
                    ch,
                    ..Cell::default()
                })
                .collect()
        };
        let ops = vec![
            Op::Reset { cols: 40, lines: 2 },
            Op::Line {
                line: 1,
                cells: row("  --> src/main.rs:3:4 nope.rs:1"),
            },
        ];
        let frame = &encode_update(Revision(1), Seq(0), ops).unwrap()[0];
        let (k, body) = ipc::read_frame(&mut &frame[..]).unwrap().unwrap();
        assert!(s.on_frame(pane, k, &body, cx));
    });
    cx.run_until_parked();
    let (bounds, cell) = cx.update(|_, cx| view.read(cx).live[&pane].painted.get());
    let at = |col: f32| {
        point(
            bounds.origin.x + cell.width * (col + 0.5),
            bounds.origin.y + cell.height * 1.5,
        )
    };
    let cmd = Modifiers {
        platform: true,
        ..Modifiers::default()
    };
    let hover = |cx: &mut VisualTestContext| cx.update(|_, cx| view.read(cx).path_hover(pane));

    cx.simulate_mouse_move(at(9.), None, Modifiers::default());
    assert_eq!(hover(cx), None, "⌘ 없이는 밑줄이 없다");
    cx.simulate_mouse_move(at(9.), None, cmd);
    let underline = Some(Highlight {
        line: 1,
        column: 6,
        cols: 15,
    });
    assert_eq!(hover(cx), underline);
    cx.simulate_modifiers_change(Modifiers::default());
    assert_eq!(hover(cx), None, "⌘을 떼면 사라진다");
    cx.simulate_modifiers_change(cmd);
    assert_eq!(hover(cx), underline, "움직이지 않고 ⌘만 눌러도 나타난다");
    cx.simulate_mouse_move(at(25.), None, cmd);
    assert_eq!(hover(cx), None, "없는 파일은 링크가 아니다");

    cx.simulate_mouse_down(at(9.), MouseButton::Left, cmd);
    cx.simulate_mouse_up(at(9.), MouseButton::Left, cmd);
    cx.run_until_parked();
    let opened = layout_of(&view, cx).active_tab().focused;
    assert_eq!(layout_of(&view, cx).file(opened), Some(file.as_path()));
    let cursor = cx.update(|_, cx| {
        let state = view.read(cx).files.tabs[&opened].editor().unwrap().clone();
        state.read(cx).cursor_position()
    });
    assert_eq!(cursor, gpui_component::input::Position::new(2, 3));

    // OWE-867: 터미널 탭으로 돌아가 일반 평문 URL을 ⌘+호버하면 밑줄이 생기고 ⌘+클릭하면 브라우저로 연다.
    // 에디터를 여는 동안 숨은 터미널 칸의 연결은 끊기므로(EOF) 다시 붙인다.
    cx.simulate_keystrokes("cmd-1");
    cx.run_until_parked();
    view.update(cx, |s, cx| {
        let (ours, _theirs) = std::os::unix::net::UnixStream::pair().unwrap();
        s.attach_test(pane, krill_proto::SessionId(12), ours, cx);
        let row = |t: &str| {
            t.chars()
                .map(|ch| Cell {
                    ch,
                    ..Cell::default()
                })
                .collect()
        };
        let ops = vec![
            Op::Reset { cols: 40, lines: 2 },
            Op::Line {
                line: 1,
                cells: row("see (https://example.com/docs?x=1)."),
            },
        ];
        let frame = &encode_update(Revision(1), Seq(1), ops).unwrap()[0];
        let (k, body) = ipc::read_frame(&mut &frame[..]).unwrap().unwrap();
        assert!(s.on_frame(pane, k, &body, cx));
    });
    cx.run_until_parked();
    cx.simulate_mouse_move(at(10.), None, Modifiers::default());
    assert_eq!(hover(cx), None, "⌘ 없이는 URL 밑줄이 없다");
    cx.simulate_mouse_move(at(10.), None, cmd);
    assert_eq!(
        hover(cx),
        Some(Highlight {
            line: 1,
            column: 5,
            cols: 28,
        }),
        "평문 URL에 ⌘+호버하면 구두점을 뺀 범위에 밑줄이 생긴다"
    );
    cx.simulate_mouse_down(at(10.), MouseButton::Left, cmd);
    cx.simulate_mouse_up(at(10.), MouseButton::Left, cmd);
    cx.run_until_parked();
    assert_eq!(
        cx.opened_url().as_deref(),
        Some("https://example.com/docs?x=1")
    );
}

/// OWE-732(MK-178): `⌘⇧E` 파일 탐색기 토글 단축키와 `krill file open|diff` 요청 큐 통합 검증.
#[gpui::test]
fn toggle_explorer_shortcut_and_cli_file_open_diff_requests_work(cx: &mut TestAppContext) {
    let dir = TempDir::new("cli-explorer");
    fs::create_dir_all(dir.0.join("src")).unwrap();
    let file = dir.0.join("src/lib.rs");
    fs::write(&file, "line1\n    line2_col5\nline3\n").unwrap();
    let diff_target = dir.0.join("src/staged.rs");
    fs::write(&diff_target, "pub fn staged() {}\n").unwrap();

    let layout_file = state_path("files-cli-explorer");
    let state_dir = layout_file.parent().unwrap().to_path_buf();
    fs::create_dir_all(&state_dir).unwrap();
    let (view, cx) = open(cx, layout_file);

    // 1. `cmd-shift-e` switches right panel to `Files` tab, second press collapses, third reopens on `Files`.
    assert_eq!(
        cx.update(|_, cx| view.read(cx).diff.read(cx).active_tab()),
        krill_app::diff::PanelTab::Changes
    );
    cx.simulate_keystrokes("cmd-shift-e");
    cx.run_until_parked();
    assert!(!cx.update(|_, cx| view.read(cx).panel_collapsed));
    assert_eq!(
        cx.update(|_, cx| view.read(cx).diff.read(cx).active_tab()),
        krill_app::diff::PanelTab::Files
    );

    cx.simulate_keystrokes("cmd-shift-e");
    cx.run_until_parked();
    assert!(cx.update(|_, cx| view.read(cx).panel_collapsed));

    cx.simulate_keystrokes("cmd-shift-e");
    cx.run_until_parked();
    assert!(!cx.update(|_, cx| view.read(cx).panel_collapsed));
    assert_eq!(
        cx.update(|_, cx| view.read(cx).diff.read(cx).active_tab()),
        krill_app::diff::PanelTab::Files
    );

    // 2. Write `file-requests.json` queue (as `krill file open` and `krill file diff --staged` do)
    // and verify the running shell consumes it, opens `src/lib.rs` at line 2 col 5, and opens
    // `src/staged.rs` in the right Diff panel (`Comparison::Index`).
    let queue_json = serde_json::json!({
        "next_id": 3,
        "requests": [
            {
                "id": 1,
                "kind": "open",
                "worktree": dir.0,
                "path": file,
                "relative_path": "src/lib.rs",
                "line": 2,
                "column": 5,
                "staged": false,
                "focus": false,
                "created_at_ms": 1
            },
            {
                "id": 2,
                "kind": "diff",
                "worktree": dir.0,
                "path": diff_target,
                "relative_path": "src/staged.rs",
                "staged": true,
                "focus": false,
                "created_at_ms": 2
            }
        ]
    });
    let req_file = state_dir.join("file-requests.json");
    fs::write(&req_file, serde_json::to_vec(&queue_json).unwrap()).unwrap();

    tick(cx);

    assert!(!req_file.exists(), "file-requests.json must be consumed");
    let opened = layout_of(&view, cx).active_tab().focused;
    assert_eq!(layout_of(&view, cx).file(opened), Some(file.as_path()));
    let cursor = cx.update(|_, cx| {
        let state = view.read(cx).files.tabs[&opened].editor().unwrap().clone();
        state.read(cx).cursor_position()
    });
    assert_eq!(cursor, gpui_component::input::Position::new(1, 4));

    assert!(!cx.update(|_, cx| view.read(cx).panel_collapsed));
    assert_eq!(
        cx.update(|_, cx| view.read(cx).diff.read(cx).active_tab()),
        krill_app::diff::PanelTab::Changes
    );
    assert_eq!(
        cx.update(|_, cx| view.read(cx).diff.read(cx).comparison()),
        krill_app::diff::Comparison::Index
    );
    assert_eq!(
        cx.update(|_, cx| view.read(cx).diff.read(cx).selected_path()),
        Some("src/staged.rs".to_owned())
    );
}

#[gpui::test]
fn cmd_shift_f_toggles_search_and_match_click_moves_editor_cursor(cx: &mut TestAppContext) {
    use gpui::{Modifiers, MouseButton};

    let dir = TempDir::new("search-cmd-shift-f");
    let src_dir = dir.0.join("src");
    fs::create_dir_all(&src_dir).unwrap();
    let file = src_dir.join("target.rs");
    fs::write(
        &file,
        "fn first() {}\nfn second() {\n    let jump_here_token = 42;\n}\n",
    )
    .unwrap();

    let (view, cx) = open(cx, state_path("files-search-cmd-shift-f"));
    let root = dir.0.clone();
    cx.update(|_, cx| {
        let diff = view.read(cx).diff.clone();
        diff.update(cx, |d, cx| d.set_repo(root, cx));
    });
    cx.run_until_parked();
    assert!(!cx.update(|_, cx| view.read(cx).panel_collapsed));
    assert_eq!(
        cx.update(|_, cx| view.read(cx).diff.read(cx).active_tab()),
        krill_app::diff::PanelTab::Changes
    );

    // 1. Pressing `cmd-shift-f` switches the right panel to `PanelTab::Search`.
    cx.simulate_keystrokes("cmd-shift-f");
    tick(cx);
    assert!(!cx.update(|_, cx| view.read(cx).panel_collapsed));
    assert_eq!(
        cx.update(|_, cx| view.read(cx).diff.read(cx).active_tab()),
        krill_app::diff::PanelTab::Search
    );

    // 2. Pressing `cmd-shift-f` again while `PanelTab::Search` is active (even with search input focused) collapses the right panel.
    cx.simulate_keystrokes("cmd-shift-f");
    tick(cx);
    assert!(cx.update(|_, cx| view.read(cx).panel_collapsed));

    // 3. Pressing `cmd-shift-f` while collapsed uncollapses the right panel and opens `PanelTab::Search`.
    cx.simulate_keystrokes("cmd-shift-f");
    tick(cx);
    assert!(!cx.update(|_, cx| view.read(cx).panel_collapsed));
    assert_eq!(
        cx.update(|_, cx| view.read(cx).diff.read(cx).active_tab()),
        krill_app::diff::PanelTab::Search
    );

    // 4. Search for `"jump_here_token"` and click the match row -> opens `src/target.rs` in the center editor at 1-based (line 3, col 9) = 0-based (2, 8).
    cx.update(|_, cx| {
        let diff = view.read(cx).diff.clone();
        diff.update(cx, |d, cx| {
            d.set_search_query("jump_here_token", cx);
        });
    });
    tick(cx);

    let match_bounds = cx
        .debug_bounds("search-match-src/target.rs-3-9")
        .expect("match row for src/target.rs:3:9 must be rendered");
    cx.simulate_mouse_down(
        match_bounds.center(),
        MouseButton::Left,
        Modifiers::default(),
    );
    cx.simulate_mouse_up(
        match_bounds.center(),
        MouseButton::Left,
        Modifiers::default(),
    );
    tick(cx);
    tick(cx);

    let opened = layout_of(&view, cx).active_tab().focused;
    assert_eq!(layout_of(&view, cx).file(opened), Some(file.as_path()));
    let cursor = cx.update(|_, cx| {
        let state = view.read(cx).files.tabs[&opened].editor().unwrap().clone();
        state.read(cx).cursor_position()
    });
    assert_eq!(cursor, gpui_component::input::Position::new(2, 8));
}
