use std::fs;

use gpui::{Entity, TestAppContext, VisualTestContext};

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

#[test]
fn file_request_lock_contention_defers_without_consuming_the_queue() {
    let dir = TempDir::new("queue-lock-contention");
    let queue = dir.0.join(FILE_REQUESTS_FILE);
    fs::write(
        &queue,
        br#"{"requests":[{"id":1,"kind":"open","worktree":"/repo","path":"/repo/a.rs","relative_path":"a.rs"}]}"#,
    )
    .unwrap();
    let lock = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(dir.0.join(FILE_REQUESTS_LOCK))
        .unwrap();
    lock.lock().unwrap();
    let path = dir.0.clone();
    let (send, recv) = std::sync::mpsc::channel();
    let worker = std::thread::spawn(move || {
        send.send(take_pending_file_requests(&path)).unwrap();
    });
    // 구 blocking lock에서도 단언 전에 잠금을 풀어 테스트가 영구 정지하지 않게 한다.
    let deferred = recv.recv_timeout(Duration::from_secs(1));
    let preserved = queue.exists();
    lock.unlock().unwrap();
    worker.join().unwrap();
    assert!(deferred.unwrap().is_empty(), "busy queue must defer");
    assert!(preserved, "busy queue must remain for the next poll");
    let requests = take_pending_file_requests(&dir.0);
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].id, 1);
    assert!(!queue.exists());
    assert!(take_pending_file_requests(&dir.0).is_empty());
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
    cx.simulate_keystrokes("ctrl-1");
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
    cx.run_until_parked();
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
    vcx.simulate_keystrokes("ctrl-1");
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
        s.new_folder_tab_on(
            s.launch_host.clone(),
            &root,
            krill_proto::control::Program::Login,
            cx,
        );
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
    cx.run_until_parked();
    let underline = Some(Highlight {
        line: 1,
        column: 6,
        cols: 15,
    });
    assert_eq!(
        hover(cx),
        underline,
        "마우스 보고가 꺼져 있으면 ⌘ 없이 마우스만 올려도 밑줄과 포인터 커서가 표시된다"
    );
    view.update(cx, |s, cx| {
        use krill_proto::screen::Msg as _;
        use krill_proto::term_modes::Modes;
        let frame = Modes {
            flags: Modes::MOUSE_CLICK,
            ..Modes::default()
        }
        .encode()
        .unwrap();
        let (k, body) = ipc::read_frame(&mut &frame[..]).unwrap().unwrap();
        assert!(s.on_frame(pane, k, &body, cx));
    });
    cx.simulate_mouse_move(at(9.), None, Modifiers::default());
    assert_eq!(
        hover(cx),
        None,
        "마우스 보고가 켜진 TUI에서는 ⌘ 없이는 밑줄이 없다"
    );
    cx.simulate_mouse_move(at(9.), None, cmd);
    assert_eq!(hover(cx), underline);
    cx.simulate_modifiers_change(Modifiers::default());
    assert_eq!(hover(cx), None, "마우스 보고 중 ⌘을 떼면 사라진다");
    cx.simulate_modifiers_change(cmd);
    assert_eq!(hover(cx), underline, "움직이지 않고 ⌘만 눌러도 나타난다");
    cx.simulate_mouse_move(at(25.), None, cmd);
    assert_eq!(hover(cx), None, "없는 파일은 링크가 아니다");
    view.update(cx, |s, cx| {
        use krill_proto::screen::Msg as _;
        use krill_proto::term_modes::Modes;
        let frame = Modes::default().encode().unwrap();
        let (k, body) = ipc::read_frame(&mut &frame[..]).unwrap().unwrap();
        assert!(s.on_frame(pane, k, &body, cx));
    });

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
    cx.simulate_keystrokes("ctrl-1");
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
        let frame = &encode_update(Revision(1), Seq(0), ops).unwrap()[0];
        let (k, body) = ipc::read_frame(&mut &frame[..]).unwrap().unwrap();
        assert!(s.on_frame(pane, k, &body, cx));
    });
    cx.run_until_parked();
    let url = Highlight {
        line: 1,
        column: 5,
        cols: 28,
    };
    cx.simulate_mouse_move(at(10.), None, Modifiers::default());
    assert_eq!(hover(cx), Some(url), "평문 URL은 ⌘ 없이도 밑줄이 생긴다");
    // 끌어서 선택하면 열지 않고, 끌지 않은 맨 클릭은 팝오버를 띄워 브라우저 열기를 고르게 한다.
    cx.simulate_mouse_down(at(10.), MouseButton::Left, Modifiers::default());
    cx.simulate_mouse_move(at(20.), Some(MouseButton::Left), Modifiers::default());
    cx.simulate_mouse_up(at(20.), MouseButton::Left, Modifiers::default());
    cx.run_until_parked();
    assert_eq!(cx.opened_url(), None, "끌어 선택한 것은 링크 열기가 아니다");
    cx.simulate_mouse_down(at(10.), MouseButton::Left, Modifiers::default());
    cx.simulate_mouse_up(at(10.), MouseButton::Left, Modifiers::default());
    cx.run_until_parked();
    assert_eq!(
        cx.opened_url(),
        None,
        "맨 클릭은 바로 열지 않고 팝오버를 띄운다"
    );
    let open = cx.debug_bounds("terminal-link-open").unwrap().center();
    cx.simulate_click(open, Modifiers::default());
    cx.run_until_parked();
    assert_eq!(
        cx.opened_url().as_deref(),
        Some("https://example.com/docs?x=1")
    );
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

/// OWE-874-1: 줄:열 지정 열기(`open_file_at`)는 미리보기 종류(마크다운)도 편집 모드로 열고 커서를 옮긴다.
#[gpui::test]
fn goto_opens_preview_kind_in_edit_mode_with_cursor(cx: &mut TestAppContext) {
    let dir = TempDir::new("goto-preview");
    let md = dir.0.join("doc.md");
    fs::write(&md, "# t\nline2 here\nline3\n").unwrap();
    let (view, cx) = open(cx, state_path("files-goto-preview"));
    cx.update(|_, cx| {
        view.update(cx, |s, cx| s.open_file_at(md.clone(), 2, 3, cx));
    });
    cx.run_until_parked();
    let pane = layout_of(&view, cx).active_tab().focused;
    assert_eq!(layout_of(&view, cx).file(pane), Some(md.as_path()));
    let editing = cx.update(|_, cx| {
        matches!(
            view.read(cx).files.tabs[&pane].body,
            Body::Text { shown: None, .. }
        )
    });
    assert!(editing, "goto는 미리보기 종류도 편집 모드로 연다");
    let cursor = cx.update(|_, cx| {
        let state = view.read(cx).files.tabs[&pane].editor().unwrap().clone();
        state.read(cx).cursor_position()
    });
    assert_eq!(cursor, gpui_component::input::Position::new(1, 2));
}

/// OWE-874-2: SVG '이미지' 미리보기는 디스크가 아니라 지금 에디터 버퍼로 그린다(미저장 수정 반영).
#[gpui::test]
fn svg_preview_reflects_unsaved_buffer(cx: &mut TestAppContext) {
    let dir = TempDir::new("svg-buffer");
    let svg = dir.0.join("icon.svg");
    let saved = "<svg xmlns=\"http://www.w3.org/2000/svg\"></svg>\n";
    fs::write(&svg, saved).unwrap();
    let edited = "<svg xmlns=\"http://www.w3.org/2000/svg\"><g id=\"edited\"></g></svg>";
    let (view, cx) = open(cx, state_path("files-svg-buffer"));
    let pane = open_file(&view, &svg, cx);
    let is_image = cx.update(|_, cx| {
        matches!(
            view.read(cx).files.tabs[&pane].body,
            Body::Text {
                shown: Some(Shown::Image(_)),
                ..
            }
        )
    });
    assert!(is_image, "SVG는 이미지 미리보기로 열린다");

    // 편집 모드로 바꿔 버퍼를 고친 뒤(저장 안 함) 미리보기로 돌아온다.
    cx.update(|window, cx| view.update(cx, |s, cx| s.set_preview(pane, false, window, cx)));
    cx.run_until_parked();
    cx.update(|window, cx| {
        view.update(cx, |s, cx| {
            let state = s.files.tabs[&pane].editor().unwrap().clone();
            state.update(cx, |e, cx| e.set_value(edited, window, cx));
        });
    });
    cx.run_until_parked();
    cx.update(|window, cx| view.update(cx, |s, cx| s.set_preview(pane, true, window, cx)));
    cx.run_until_parked();

    let bytes = cx.update(|_, cx| match &view.read(cx).files.tabs[&pane].body {
        Body::Text {
            shown: Some(Shown::Image(view)),
            ..
        } => view.read(cx).svg_bytes(),
        _ => None,
    });
    assert_eq!(
        bytes.as_deref().map(String::from_utf8_lossy),
        Some(std::borrow::Cow::Borrowed(edited)),
        "미리보기는 디스크가 아니라 버퍼를 그린다"
    );
    assert_eq!(
        fs::read(&svg).unwrap(),
        saved.as_bytes(),
        "디스크는 그대로다"
    );
}

/// OWE-874-3: 에디터 파일 탭이 포커스된 상태에서 ⌘F는 터미널 검색 바가 아니라 에디터 내장 검색을 연다.
#[gpui::test]
fn cmd_f_in_editor_opens_editor_search(cx: &mut TestAppContext) {
    let dir = TempDir::new("editor-search");
    let file = dir.0.join("main.rs");
    fs::write(&file, "fn main() {}\n").unwrap();
    let (view, cx) = open(cx, state_path("files-editor-search"));
    let pane = open_file(&view, &file, cx);
    cx.simulate_keystrokes("cmd-f");
    cx.run_until_parked();
    assert!(
        !cx.update(|_, cx| view.read(cx).search.is_some()),
        "에디터 탭에서는 터미널 검색 바를 띄우지 않는다"
    );
    assert!(
        cx.update(|_, cx| {
            let state = view.read(cx).files.tabs[&pane].editor().unwrap().clone();
            state.read(cx).search_session().open
        }),
        "에디터 내장 검색이 열린다"
    );
}

/// OWE-874-4: 창보다 긴 줄도 잘리지 않고(자동 줄 바꿈) 끝까지 편집할 수 있다.
#[gpui::test]
fn long_line_wraps_and_cursor_reaches_end(cx: &mut TestAppContext) {
    let dir = TempDir::new("long-line");
    let file = dir.0.join("long.txt");
    let long: String = "x".repeat(600);
    fs::write(&file, format!("{long}\n")).unwrap();
    let (view, cx) = open(cx, state_path("files-long-line"));
    let pane = open_file(&view, &file, cx);
    assert_eq!(editor_text(&view, pane, cx), format!("{long}\n"));
    // 600번째 글자(0-based 599)로 커서를 옮긴다 — 잘려서 못 가는 일이 없다.
    cx.update(|window, cx| {
        view.update(cx, |s, cx| {
            let state = s.files.tabs[&pane].editor().unwrap().clone();
            state.update(cx, |e, cx| {
                e.set_cursor_position(gpui_component::input::Position::new(0, 599), window, cx);
            });
        });
    });
    let cursor = cx.update(|_, cx| {
        let state = view.read(cx).files.tabs[&pane].editor().unwrap().clone();
        state.read(cx).cursor_position()
    });
    assert_eq!(cursor, gpui_component::input::Position::new(0, 599));
}

#[gpui::test]
fn explicit_close_bulk_waits_for_unsaved_confirmation_before_terminating(cx: &mut TestAppContext) {
    let dir = TempDir::new("close-sessions-after-unsaved");
    let file = dir.0.join("dirty.txt");
    fs::write(&file, "text\n").unwrap();
    let (view, cx) = open(cx, state_path("close-sessions-after-unsaved"));
    let requests = Arc::new(std::sync::Mutex::new(Vec::new()));
    let recorded = requests.clone();
    view.update(cx, |s, _| {
        s.close_call = Some(Arc::new(move |request| {
            if matches!(request, krill_proto::control::Request::List) {
                return Ok(krill_proto::control::Reply::Sessions(Vec::new()));
            }
            recorded.lock().unwrap().push(request.clone());
            Ok(krill_proto::control::Reply::Closed)
        }));
        let pane = s.layout.active_tab().focused;
        s.layout.set_session(pane, krill_proto::SessionId(107));
    });
    let pane = open_file(&view, &file, cx);
    cx.simulate_input("draft");
    assert!(dirty(&view, pane, cx));
    cx.simulate_keystrokes("cmd-t");
    view.update(cx, |s, cx| s.close_other_tabs(s.layout.active(), cx));
    cx.run_until_parked();
    assert!(cx.has_pending_prompt());
    assert!(requests.lock().unwrap().is_empty());
    cx.simulate_prompt_answer(etext::CANCEL.get());
    cx.run_until_parked();
    assert!(requests.lock().unwrap().is_empty());
    assert!(
        layout_of(&view, cx)
            .sessions()
            .contains(&krill_proto::SessionId(107))
    );
    view.update(cx, |s, cx| s.close_other_tabs(s.layout.active(), cx));
    cx.run_until_parked();
    cx.simulate_prompt_answer(etext::DISCARD.get());
    cx.run_until_parked();
    assert_eq!(
        *requests.lock().unwrap(),
        vec![krill_proto::control::Request::Close {
            session: krill_proto::SessionId(107)
        }]
    );
}

/// The file worker is deliberately held while UI actions and drawing continue.
#[gpui::test]
fn slow_open_keeps_ui_responsive_and_rejects_replaced_preview(cx: &mut TestAppContext) {
    let dir = TempDir::new("async-stale-open");
    let old = dir.0.join("old.txt");
    let new = dir.0.join("new.txt");
    fs::write(&old, "old").unwrap();
    fs::write(&new, "new").unwrap();
    let (view, cx) = open(cx, state_path("async-stale-open"));
    let (release, gate) = tokio::sync::oneshot::channel();
    view.update(cx, |shell, cx| {
        shell.files.next_io = Some(gate);
        shell.open_file_preview(old.clone(), cx);
    });
    cx.run_until_parked();
    let pane = layout_of(&view, cx).active_tab().focused;
    assert!(cx.update(|_, cx| view.read(cx).files.loading.contains_key(&pane)));
    assert!(
        cx.debug_bounds("file-pane").is_some(),
        "draws while disk work is pending"
    );
    view.update(cx, |shell, cx| shell.open_file_preview(new.clone(), cx));
    cx.run_until_parked();
    assert_eq!(layout_of(&view, cx).active_tab().focused, pane);
    assert_eq!(editor_text(&view, pane, cx), "new");
    release.send(()).unwrap();
    cx.run_until_parked();
    assert_eq!(
        editor_text(&view, pane, cx),
        "new",
        "late IO must not replace the new preview"
    );
}

#[gpui::test]
fn edits_during_slow_save_stay_dirty_and_close_waits_for_completion(cx: &mut TestAppContext) {
    let dir = TempDir::new("async-save-edit");
    let file = dir.0.join("file.txt");
    fs::write(&file, "initial").unwrap();
    let (view, cx) = open(cx, state_path("async-save-edit"));
    let pane = open_file(&view, &file, cx);
    cx.simulate_keystrokes("x");
    cx.run_until_parked();
    let (release, gate) = tokio::sync::oneshot::channel();
    view.update(cx, |shell, cx| {
        shell.files.next_io = Some(gate);
        shell.save_file(pane, cx).detach();
    });
    cx.run_until_parked();
    cx.simulate_keystrokes("y");
    cx.run_until_parked();
    assert_eq!(editor_text(&view, pane, cx), "xyinitial");
    assert!(cx.update(|_, cx| view.read(cx).files.tabs[&pane].saving));
    let closed = cx.update(|_, cx| {
        view.update(cx, |_, cx| {
            Shell::resolve_unsaved(async { Some(0) }, &[pane], cx)
        })
    });
    assert!(
        !cx.foreground_executor().block_test(closed),
        "inflight save cannot authorize close"
    );
    release.send(()).unwrap();
    cx.run_until_parked();
    assert_eq!(fs::read_to_string(&file).unwrap(), "xinitial");
    assert!(dirty(&view, pane, cx));
    cx.simulate_keystrokes("cmd-s");
    cx.run_until_parked();
    assert_eq!(fs::read_to_string(&file).unwrap(), "xyinitial");
    assert!(!dirty(&view, pane, cx));
}

#[gpui::test]
fn late_reload_does_not_discard_new_input(cx: &mut TestAppContext) {
    let dir = TempDir::new("async-reload-edit");
    let file = dir.0.join("file.txt");
    fs::write(&file, "initial").unwrap();
    let (view, cx) = open(cx, state_path("async-reload-edit"));
    let pane = open_file(&view, &file, cx);
    fs::write(&file, "external change").unwrap();
    let (release, gate) = tokio::sync::oneshot::channel();
    cx.update(|window, cx| {
        view.update(cx, |shell, cx| {
            shell.files.next_io = Some(gate);
            shell.reload_file(pane, window, cx);
        });
    });
    cx.run_until_parked();
    cx.simulate_keystrokes("x");
    cx.run_until_parked();
    release.send(()).unwrap();
    cx.run_until_parked();
    assert_eq!(editor_text(&view, pane, cx), "xinitial");
    assert!(dirty(&view, pane, cx));
}

#[gpui::test]
fn accepted_save_survives_tab_discard_and_quit_barrier_waits(cx: &mut TestAppContext) {
    let dir = TempDir::new("async-save-discard");
    let file = dir.0.join("file.txt");
    fs::write(&file, "initial").unwrap();
    let (view, cx) = open(cx, state_path("async-save-discard"));
    let pane = open_file(&view, &file, cx);
    cx.simulate_keystrokes("x");
    cx.run_until_parked();
    let (release, gate) = tokio::sync::oneshot::channel();
    view.update(cx, |shell, cx| {
        shell.files.next_io = Some(gate);
        shell.save_file(pane, cx).detach();
        shell.files.tabs.remove(&pane);
        shell.change(true, cx, Layout::close_pane);
    });
    let finished = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let observed = finished.clone();
    cx.update(|_, cx| {
        let barrier = flush_editor_writes(cx);
        cx.spawn(async move |cx| {
            barrier.await;
            observed.store(true, std::sync::atomic::Ordering::SeqCst);
            let _ = cx;
        })
        .detach();
    });
    cx.run_until_parked();
    assert!(!finished.load(std::sync::atomic::Ordering::SeqCst));
    release.send(()).unwrap();
    cx.run_until_parked();
    assert!(finished.load(std::sync::atomic::Ordering::SeqCst));
    assert_eq!(fs::read_to_string(file).unwrap(), "xinitial");
}

/// 아티팩트 공유: .md·.html 파일 머리에만 공유 버튼이 있다. 저장 안 된 변경이 있으면 저장 안내를,
/// 공개 공유가 꺼져 있으면 CLI를 부르지 않고 설정 > 아티팩트를 연다.
#[gpui::test]
fn share_button_only_on_shareable_files_and_dirty_changes_block_upload(cx: &mut TestAppContext) {
    let dir = TempDir::new("share-artifact");
    let md = dir.0.join("doc.md");
    fs::write(&md, "# doc\n").unwrap();
    let rs = dir.0.join("main.rs");
    fs::write(&rs, "fn main() {}\n").unwrap();
    let (view, cx) = open(cx, state_path("files-share-artifact"));

    open_file(&view, &rs, cx);
    cx.update(|window, cx| window.draw(cx).clear(cx));
    assert!(cx.debug_bounds("file-share-artifact").is_none());

    let pane = open_file(&view, &md, cx);
    cx.update(|window, cx| window.draw(cx).clear(cx));
    assert!(cx.debug_bounds("file-share-artifact").is_some());

    cx.update(|window, cx| view.update(cx, |s, cx| s.set_preview(pane, false, window, cx)));
    cx.run_until_parked();
    cx.simulate_keystrokes("x");
    assert!(dirty(&view, pane, cx));
    cx.update(|window, cx| view.update(cx, |s, cx| s.share_file_artifact(pane, window, cx)));
    cx.update(|_, cx| {
        let s = view.read(cx);
        assert_eq!(s.notice.as_deref(), Some(atext::SAVE_FIRST.get()));
        assert!(s.settings.is_none());
    });
}

#[gpui::test]
fn crlf_baseline_stays_clean_on_open_and_reload(cx: &mut TestAppContext) {
    let dir = TempDir::new("crlf-baseline");
    let file = dir.0.join("source.txt");
    fs::write(&file, "first\r\nsecond\r\n").unwrap();
    let (view, cx) = open(cx, state_path("crlf-baseline"));
    let pane = open_file(&view, &file, cx);
    assert_eq!(editor_text(&view, pane, cx), "first\r\nsecond\r\n");
    assert!(!dirty(&view, pane, cx));
    fs::write(&file, "third\r\nfourth\r\n").unwrap();
    cx.update(|window, cx| view.update(cx, |shell, cx| shell.reload_file(pane, window, cx)));
    cx.run_until_parked();
    assert_eq!(editor_text(&view, pane, cx), "third\r\nfourth\r\n");
    assert!(!dirty(&view, pane, cx));
}

#[gpui::test]
fn hidden_clean_cache_evicts_but_dirty_and_selected_documents_survive(cx: &mut TestAppContext) {
    let dir = TempDir::new("clean-cache-pins");
    let (view, cx) = open(cx, state_path("clean-cache-pins"));
    let dirty_path = dir.0.join("dirty.txt");
    fs::write(&dirty_path, "unsaved baseline").unwrap();
    let dirty_pane = open_file(&view, &dirty_path, cx);
    view.update(cx, |shell, _| {
        shell
            .files
            .tabs
            .get_mut(&dirty_pane)
            .unwrap()
            .doc
            .as_mut()
            .unwrap()
            .edited(false)
    });
    let selected_path = dir.0.join("selected.txt");
    fs::write(&selected_path, "selected text").unwrap();
    let selected = open_file(&view, &selected_path, cx);
    cx.update(|window, cx| {
        let editor = view.read(cx).files.tabs[&selected]
            .editor()
            .unwrap()
            .clone();
        editor.update(cx, |editor, cx| editor.select_all(window, cx));
    });
    let oldest_path = dir.0.join("oldest.txt");
    fs::write(&oldest_path, "old content").unwrap();
    let oldest = open_file(&view, &oldest_path, cx);
    for index in 0..CLEAN_CACHE_TABS + 3 {
        let path = dir.0.join(format!("{index}.txt"));
        fs::write(&path, "small document").unwrap();
        open_file(&view, &path, cx);
    }
    view.read_with(cx, |shell, _| {
        assert!(shell.files.tabs.contains_key(&dirty_pane));
        assert!(shell.files.tabs.contains_key(&selected));
        assert!(!shell.files.tabs.contains_key(&oldest));
        assert!(shell.files.tabs.len() <= CLEAN_CACHE_TABS);
    });
    fs::write(&oldest_path, "fresh content after eviction").unwrap();
    let reopened = open_file(&view, &oldest_path, cx);
    assert_eq!(
        editor_text(&view, reopened, cx),
        "fresh content after eviction"
    );
}

#[gpui::test]
fn undo_to_original_content_finishes_coalesced_dirty_comparison(cx: &mut TestAppContext) {
    let dir = TempDir::new("coalesced-undo");
    let path = dir.0.join("source.txt");
    fs::write(&path, "original\r\n").unwrap();
    let (view, cx) = open(cx, state_path("coalesced-undo"));
    let pane = open_file(&view, &path, cx);
    cx.simulate_input("x");
    cx.run_until_parked();
    assert!(dirty(&view, pane, cx));
    cx.simulate_keystrokes("cmd-z");
    cx.run_until_parked();
    assert_eq!(editor_text(&view, pane, cx), "original\r\n");
    assert!(!dirty(&view, pane, cx));
    view.read_with(cx, |shell, _| assert!(shell.files.comparing.is_empty()));
}

#[gpui::test]
fn saving_hidden_dirty_tabs_releases_cache_pins_without_switching_tabs(cx: &mut TestAppContext) {
    let dir = TempDir::new("cache-save-unpin");
    let (view, cx) = open(cx, state_path("cache-save-unpin"));
    for index in 0..CLEAN_CACHE_TABS + 5 {
        let path = dir.0.join(format!("{index}.txt"));
        fs::write(&path, "document").unwrap();
        let pane = open_file(&view, &path, cx);
        view.update(cx, |shell, _| {
            shell
                .files
                .tabs
                .get_mut(&pane)
                .unwrap()
                .doc
                .as_mut()
                .unwrap()
                .edited(false);
        });
    }
    let focused = view.read_with(cx, |shell, _| {
        assert!(shell.files.tabs.len() > CLEAN_CACHE_TABS);
        shell.layout.focused()
    });
    view.update(cx, |shell, cx| {
        let panes: Vec<_> = shell.files.tabs.keys().copied().collect();
        for pane in panes {
            shell.save_file(pane, cx).detach();
        }
    });
    cx.run_until_parked();
    view.read_with(cx, |shell, _| {
        assert_eq!(shell.layout.focused(), focused);
        assert!(shell.files.tabs.len() <= CLEAN_CACHE_TABS);
        assert!(
            shell
                .files
                .tabs
                .values()
                .all(|tab| !tab.is_dirty() && !tab.saving)
        );
    });
}

#[gpui::test]
fn clearing_hidden_selection_releases_cache_pin_without_switching_tabs(cx: &mut TestAppContext) {
    let dir = TempDir::new("cache-selection-unpin");
    let (view, cx) = open(cx, state_path("cache-selection-unpin"));
    let mut editors = Vec::new();
    for index in 0..CLEAN_CACHE_TABS + 3 {
        let path = dir.0.join(format!("{index}.txt"));
        fs::write(&path, "selected text").unwrap();
        let pane = open_file(&view, &path, cx);
        cx.update(|window, cx| {
            let editor = view.read(cx).files.tabs[&pane].editor().unwrap().clone();
            editor.update(cx, |editor, cx| editor.select_all(window, cx));
            editors.push(editor);
        });
        cx.run_until_parked();
    }
    let focused = view.read_with(cx, |shell, _| {
        assert!(shell.files.tabs.len() > CLEAN_CACHE_TABS);
        shell.layout.focused()
    });
    for editor in editors {
        cx.update(|_, cx| editor.update(cx, |editor, cx| editor.set_selected_range(0..0, cx)));
    }
    cx.run_until_parked();
    view.read_with(cx, |shell, _| {
        assert_eq!(shell.layout.focused(), focused);
        assert!(shell.files.tabs.len() <= CLEAN_CACHE_TABS);
    });
}

#[gpui::test]
fn global_quit_after_last_window_waits_for_app_owned_pending_writes(cx: &mut TestAppContext) {
    let (_view, visual) = open(cx, state_path("windowless-quit-writes"));
    visual.update(|window, _| window.remove_window());
    let (release, gate) = tokio::sync::oneshot::channel();
    let quitting = std::rc::Rc::new(std::cell::Cell::new(false));
    cx.update(|cx| {
        assert!(cx.windows().is_empty());
        cx.set_global(PendingEditorWrites(Some(gate)));
        cx.set_global(QuitObserved(quitting.clone()));
        cx.dispatch_action(&Quit);
    });
    cx.run_until_parked();
    assert!(
        !quitting.get(),
        "queued writes must finish before GPUI shutdown starts"
    );
    release.send(()).unwrap();
    cx.run_until_parked();
    assert!(quitting.get());
}

#[gpui::test]
fn closing_window_materializes_latest_pending_layout_without_waiting_for_debounce(
    cx: &mut TestAppContext,
) {
    let path = state_path("close-ui-layout-debounce");
    let (view, visual) = open(cx, path.clone());
    let (before, expected) = view.update(visual, |shell, _| {
        shell.flush_layout();
        let before = shell.layout_save_requests.snapshots();
        for width in 240_u16..320 {
            shell.layout.sidebar_width = f32::from(width);
            shell.save();
        }
        (before, shell.layout.clone())
    });
    visual.update(|window, cx| view.update(cx, |shell, cx| shell.request_close(window, cx)));
    visual.run_until_parked();
    assert!(cx.update(|cx| cx.windows().is_empty()));
    assert_eq!(Layout::load(&path).unwrap(), expected);
    view.read_with(cx, |shell, _| {
        assert_eq!(shell.layout_save_requests.snapshots(), before + 1);
    });
    cx.executor()
        .advance_clock(super::super::persist::DELAY * 2);
    cx.run_until_parked();
    view.read_with(cx, |shell, _| {
        assert_eq!(shell.layout_save_requests.snapshots(), before + 1);
    });
}

#[gpui::test]
fn quit_preflush_materializes_latest_layout_before_shutdown_waits(cx: &mut TestAppContext) {
    let path = state_path("quit-ui-layout-debounce");
    let (view, visual) = open(cx, path.clone());
    let (before, expected) = view.update(visual, |shell, _| {
        shell.flush_layout();
        let before = shell.layout_save_requests.snapshots();
        for width in 260_u16..340 {
            shell.layout.panel_width = f32::from(width);
            shell.save();
        }
        (before, shell.layout.clone())
    });
    let quitting = std::rc::Rc::new(std::cell::Cell::new(false));
    // No UI timer advance: quit must materialize the last pending request now.
    visual.update(|_, cx| {
        cx.set_global(QuitObserved(quitting.clone()));
        quit_app(cx);
    });
    visual.run_until_parked();
    view.read_with(visual, |shell, _| {
        assert_eq!(shell.layout_save_requests.snapshots(), before + 1);
        assert!(!shell.layout_save_requests.is_pending());
        shell.saver.flush();
        let persisted = Layout::load(&path).unwrap();
        assert_eq!(persisted, shell.layout);
        assert_eq!(
            persisted.panel_width.to_bits(),
            expected.panel_width.to_bits()
        );
        assert!(shell.quit_layout_ready.get());
    });
    visual.run_until_parked();
    assert!(
        quitting.get(),
        "platform quit is requested only after durable preflush"
    );
    view.read_with(visual, |shell, _| {
        assert_eq!(Layout::load(&path).unwrap(), shell.layout);
    });
}

/// A comparison never edits either version; Cancel and Escape return to the draft.
#[gpui::test]
fn reload_comparison_is_readonly_and_cancel_preserves_draft(cx: &mut TestAppContext) {
    let dir = TempDir::new("reload-compare-cancel");
    let file = dir.0.join("file.txt");
    fs::write(&file, "original").unwrap();
    let (view, cx) = open(cx, state_path("reload-compare-cancel"));
    let pane = open_file(&view, &file, cx);
    cx.simulate_input("draft ");
    fs::write(&file, "external version").unwrap();
    tick(cx);
    let button = cx.debug_bounds("file-reload").unwrap();
    cx.simulate_click(button.center(), gpui::Modifiers::default());
    cx.run_until_parked();
    assert!(cx.debug_bounds("file-comparison").is_some());
    assert!(cx.debug_bounds("file-compare-draft").is_some());
    assert!(cx.debug_bounds("file-compare-disk").is_some());
    cx.simulate_input("must not change comparison");
    cx.run_until_parked();
    assert_eq!(editor_text(&view, pane, cx), "draft original");
    view.read_with(cx, |shell, cx| {
        let comparison = shell.files.tabs[&pane].comparison.as_ref().unwrap();
        assert_eq!(
            comparison.draft.read(cx).text().to_string(),
            "draft original"
        );
        assert_eq!(
            comparison.disk.read(cx).text().to_string(),
            "external version"
        );
    });
    let cancel = cx.debug_bounds("file-compare-cancel").unwrap();
    cx.simulate_click(cancel.center(), gpui::Modifiers::default());
    cx.run_until_parked();
    assert!(cx.debug_bounds("file-comparison").is_none());
    assert!(dirty(&view, pane, cx));
    cx.update(|window, cx| view.update(cx, |shell, cx| shell.compare_file(pane, window, cx)));
    cx.run_until_parked();
    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    assert!(cx.debug_bounds("file-comparison").is_none());
    assert_eq!(fs::read_to_string(file).unwrap(), "external version");
}

/// Reload clears the native Undo history, so recovery is explicit and uses the new disk baseline.
#[gpui::test]
fn reload_restore_draft_keeps_disk_baseline_and_cursor(cx: &mut TestAppContext) {
    let dir = TempDir::new("reload-recovery-baseline");
    let file = dir.0.join("file.txt");
    fs::write(&file, "old saved content").unwrap();
    let (view, cx) = open(cx, state_path("reload-recovery-baseline"));
    let pane = open_file(&view, &file, cx);
    cx.simulate_input("draft ");
    let cursor = view.read_with(cx, |shell, cx| {
        shell.files.tabs[&pane]
            .editor()
            .unwrap()
            .read(cx)
            .cursor_position()
    });
    fs::write(&file, "new disk").unwrap();
    cx.update(|window, cx| view.update(cx, |shell, cx| shell.compare_file(pane, window, cx)));
    cx.run_until_parked();
    let apply = cx.debug_bounds("file-compare-apply").unwrap();
    cx.simulate_click(apply.center(), gpui::Modifiers::default());
    cx.run_until_parked();
    assert_eq!(editor_text(&view, pane, cx), "new disk");
    assert!(!dirty(&view, pane, cx));
    assert!(cx.debug_bounds("file-restore-draft").is_some());
    cx.simulate_keystrokes("cmd-z");
    cx.run_until_parked();
    assert_eq!(
        editor_text(&view, pane, cx),
        "new disk",
        "reload starts a fresh Undo history"
    );
    let restore = cx.debug_bounds("file-restore-draft").unwrap();
    cx.simulate_click(restore.center(), gpui::Modifiers::default());
    cx.run_until_parked();
    assert_eq!(editor_text(&view, pane, cx), "draft old saved content");
    assert!(dirty(&view, pane, cx));
    view.read_with(cx, |shell, cx| {
        let tab = &shell.files.tabs[&pane];
        assert_eq!(tab.doc.as_ref().unwrap().saved(), "new disk");
        assert_eq!(tab.editor().unwrap().read(cx).cursor_position(), cursor);
        assert!(tab.recovery.is_none());
    });
    assert_eq!(fs::read_to_string(&file).unwrap(), "new disk");
    // Undoing this edit back to the restored draft must still be dirty.
    cx.simulate_input("x");
    cx.simulate_keystrokes("cmd-z");
    cx.run_until_parked();
    assert_eq!(editor_text(&view, pane, cx), "draft old saved content");
    assert!(dirty(&view, pane, cx));
    cx.simulate_keystrokes("cmd-s");
    cx.run_until_parked();
    assert_eq!(fs::read_to_string(file).unwrap(), "draft old saved content");
    assert!(!dirty(&view, pane, cx));
}

#[gpui::test]
fn reload_recovery_of_clean_buffer_is_dirty_and_new_edits_disable_restore(cx: &mut TestAppContext) {
    let dir = TempDir::new("reload-clean-recovery");
    let file = dir.0.join("file.txt");
    fs::write(&file, "old clean").unwrap();
    let (view, cx) = open(cx, state_path("reload-clean-recovery"));
    let pane = open_file(&view, &file, cx);
    fs::write(&file, "new disk").unwrap();
    tick(cx);
    cx.update(|window, cx| {
        view.update(cx, |shell, cx| shell.restore_reload_draft(pane, window, cx));
    });
    cx.run_until_parked();
    assert_eq!(editor_text(&view, pane, cx), "old clean");
    assert!(
        dirty(&view, pane, cx),
        "old clean content differs from current disk"
    );
    cx.update(|window, cx| view.update(cx, |shell, cx| shell.reload_file(pane, window, cx)));
    cx.run_until_parked();
    cx.simulate_input("new edits ");
    cx.run_until_parked();
    let edited = editor_text(&view, pane, cx);
    cx.update(|window, cx| {
        view.update(cx, |shell, cx| shell.restore_reload_draft(pane, window, cx));
    });
    cx.run_until_parked();
    assert_eq!(editor_text(&view, pane, cx), edited);
    assert!(cx.debug_bounds("file-restore-draft").is_none());
}

#[gpui::test]
fn reload_comparison_rejects_rechanged_disk_and_io_failure(cx: &mut TestAppContext) {
    let dir = TempDir::new("reload-compare-stale");
    let file = dir.0.join("file.txt");
    fs::write(&file, "initial").unwrap();
    let (view, cx) = open(cx, state_path("reload-compare-stale"));
    let pane = open_file(&view, &file, cx);
    cx.simulate_input("draft ");
    fs::write(&file, "external version").unwrap();
    cx.update(|window, cx| view.update(cx, |shell, cx| shell.compare_file(pane, window, cx)));
    cx.run_until_parked();
    fs::write(&file, "external changed again and longer").unwrap();
    cx.update(|window, cx| {
        view.update(cx, |shell, cx| {
            shell.apply_file_comparison(pane, window, cx);
        });
    });
    cx.run_until_parked();
    assert_eq!(editor_text(&view, pane, cx), "draft initial");
    assert!(cx.debug_bounds("file-error").is_some());
    view.read_with(cx, |shell, _| {
        assert!(shell.files.tabs[&pane].recovery.is_none());
    });
    fs::remove_file(&file).unwrap();
    cx.update(|window, cx| view.update(cx, |shell, cx| shell.reload_file(pane, window, cx)));
    cx.run_until_parked();
    assert_eq!(editor_text(&view, pane, cx), "draft initial");
    assert!(dirty(&view, pane, cx));
}

#[gpui::test]
fn reload_comparison_read_races_preserve_new_edits_and_replaced_tabs(cx: &mut TestAppContext) {
    let dir = TempDir::new("reload-compare-races");
    let file = dir.0.join("file.txt");
    fs::write(&file, "initial").unwrap();
    let (view, cx) = open(cx, state_path("reload-compare-races"));
    let pane = open_file(&view, &file, cx);
    let (release, gate) = tokio::sync::oneshot::channel();
    cx.update(|window, cx| {
        view.update(cx, |shell, cx| {
            shell.files.next_io = Some(gate);
            shell.compare_file(pane, window, cx);
        });
    });
    cx.run_until_parked();
    cx.simulate_input("new edit ");
    release.send(()).unwrap();
    cx.run_until_parked();
    assert!(cx.debug_bounds("file-comparison").is_none());
    assert_eq!(editor_text(&view, pane, cx), "new edit initial");
    // Recreate the same path with identical Doc/revision but a different editor entity.
    let (release, gate) = tokio::sync::oneshot::channel();
    cx.update(|window, cx| {
        view.update(cx, |shell, cx| {
            let loaded = read_file(&file, false);
            let fresh = Shell::load_file_tab(pane, file.clone(), None, window, cx, loaded);
            shell.files.tabs.insert(pane, fresh);
            shell.files.next_io = Some(gate);
            shell.reload_file(pane, window, cx);
            let loaded = read_file(&file, false);
            let fresh = Shell::load_file_tab(pane, file.clone(), None, window, cx, loaded);
            shell.files.tabs.insert(pane, fresh);
        });
    });
    fs::write(&file, "late external read").unwrap();
    release.send(()).unwrap();
    cx.run_until_parked();
    assert_eq!(editor_text(&view, pane, cx), "initial");
    view.read_with(cx, |shell, _| {
        assert!(shell.files.tabs[&pane].recovery.is_none());
    });
    // A pending comparison must not resurrect a closed tab.
    let (release, gate) = tokio::sync::oneshot::channel();
    cx.update(|window, cx| {
        view.update(cx, |shell, cx| {
            shell.files.next_io = Some(gate);
            shell.compare_file(pane, window, cx);
            shell.files.tabs.remove(&pane);
            shell.change(true, cx, Layout::close_pane);
        });
    });
    release.send(()).unwrap();
    cx.run_until_parked();
    view.read_with(cx, |shell, _| {
        assert!(!shell.files.tabs.contains_key(&pane));
    });
}

#[gpui::test]
fn reload_comparison_cancel_pending_apply_and_repeated_reload_keep_first_draft(
    cx: &mut TestAppContext,
) {
    let dir = TempDir::new("reload-compare-cancel-pending");
    let file = dir.0.join("file.txt");
    fs::write(&file, "initial").unwrap();
    let (view, cx) = open(cx, state_path("reload-compare-cancel-pending"));
    let pane = open_file(&view, &file, cx);
    cx.simulate_input("draft ");
    fs::write(&file, "disk one").unwrap();
    cx.update(|window, cx| view.update(cx, |shell, cx| shell.compare_file(pane, window, cx)));
    cx.run_until_parked();
    let (release, gate) = tokio::sync::oneshot::channel();
    cx.update(|window, cx| {
        view.update(cx, |shell, cx| {
            shell.files.next_io = Some(gate);
            shell.apply_file_comparison(pane, window, cx);
        });
    });
    cx.run_until_parked();
    let cancel = cx.debug_bounds("file-compare-cancel").unwrap();
    cx.simulate_click(cancel.center(), gpui::Modifiers::default());
    release.send(()).unwrap();
    cx.run_until_parked();
    assert_eq!(editor_text(&view, pane, cx), "draft initial");
    assert!(dirty(&view, pane, cx));
    cx.update(|window, cx| view.update(cx, |shell, cx| shell.reload_file(pane, window, cx)));
    cx.run_until_parked();
    fs::write(&file, "disk two, newer").unwrap();
    tick(cx);
    assert_eq!(editor_text(&view, pane, cx), "disk two, newer");
    cx.update(|window, cx| {
        view.update(cx, |shell, cx| shell.restore_reload_draft(pane, window, cx));
    });
    cx.run_until_parked();
    assert_eq!(editor_text(&view, pane, cx), "draft initial");
    assert!(dirty(&view, pane, cx));
    view.read_with(cx, |shell, _| {
        assert_eq!(
            shell.files.tabs[&pane].doc.as_ref().unwrap().saved(),
            "disk two, newer"
        );
    });
}

fn show_wrapped_path(
    view: &Entity<Shell>,
    cx: &mut VisualTestContext,
    dir: &Path,
) -> (PaneId, Point<Pixels>) {
    use krill_proto::screen::{Cell, Flags, Op, Revision, Seq, encode_update};
    let pane = layout_of(view, cx).active_tab().focused;
    let (ours, _theirs) = UnixStream::pair().unwrap();
    view.update(cx, |s, cx| {
        s.layout.set_launch(
            pane,
            Create {
                account: None,
                program: Program::Login,
                cwd: dir.as_os_str().as_bytes().to_vec(),
                cols: 80,
                rows: 24,
            },
        );
        s.attach_test(pane, krill_proto::SessionId(121), ours, cx);
        let mut head: Vec<_> = "src/long-name.rs"
            .chars()
            .map(|ch| Cell {
                ch,
                ..Cell::default()
            })
            .collect();
        assert_eq!(head.len(), 16, "실제 마지막 칸에 WRAPLINE을 둔다");
        head.last_mut().unwrap().flags |= Flags::WRAPLINE;
        let ops = vec![
            Op::Reset { cols: 16, lines: 4 },
            Op::Line {
                line: 0,
                cells: head,
            },
            Op::Line {
                line: 1,
                cells: ":2:3"
                    .chars()
                    .map(|ch| Cell {
                        ch,
                        ..Cell::default()
                    })
                    .collect(),
            },
        ];
        let frame = &encode_update(Revision(1), Seq(0), ops).unwrap()[0];
        let (kind, body) = ipc::read_frame(&mut &frame[..]).unwrap().unwrap();
        assert!(s.on_frame(pane, kind, &body, cx));
    });
    cx.run_until_parked();
    let (bounds, cell) = cx.update(|_, cx| view.read(cx).live[&pane].painted.get());
    (
        pane,
        point(
            bounds.origin.x + cell.width * 2.5,
            bounds.origin.y + cell.height * 1.5,
        ),
    )
}

#[gpui::test]
fn wrapped_file_cmd_click_uses_full_location_and_rejects_changed_prefix(cx: &mut TestAppContext) {
    let dir = TempDir::new("wrapped-cmd");
    fs::create_dir_all(dir.0.join("src")).unwrap();
    let file = dir.0.join("src/long-name.rs");
    fs::write(&file, "first\nsecond\n").unwrap();
    let (view, cx) = open(cx, state_path("wrapped-cmd"));
    let (pane, at) = show_wrapped_path(&view, cx, &dir.0);
    let (tx, rx) = tokio::sync::oneshot::channel();
    view.update(cx, |s, cx| {
        use krill_proto::screen::{Cell, Flags, Op, Revision, Seq, encode_update};
        s.files.link_next_io = Some(rx);
        s.path_link_at(pane, at, Some(terminal_menu::FileAction::Open), cx);
        // 포인터 행은 두고 앞 행만 바꾼다.
        let mut cells: Vec<_> = "src/wrong-na.rs"
            .chars()
            .map(|ch| Cell {
                ch,
                ..Cell::default()
            })
            .collect();
        cells.resize(16, Cell::default());
        cells.last_mut().unwrap().flags |= Flags::WRAPLINE;
        let frame =
            &encode_update(Revision(2), Seq(0), vec![Op::Line { line: 0, cells }]).unwrap()[0];
        let (kind, body) = ipc::read_frame(&mut &frame[..]).unwrap().unwrap();
        assert!(s.on_frame(pane, kind, &body, cx));
    });
    tx.send(()).unwrap();
    cx.run_until_parked();
    assert!(
        layout_of(&view, cx).file(pane).is_none(),
        "늦은 결과로 옛 파일을 열지 않는다"
    );
    let (pane, at) = show_wrapped_path(&view, cx, &dir.0);
    cx.simulate_mouse_move(
        at,
        None,
        Modifiers {
            platform: true,
            ..Modifiers::default()
        },
    );
    cx.run_until_parked();
    assert_eq!(
        cx.update(|_, cx| view.read(cx).path_hover(pane)),
        Some(Highlight {
            line: 1,
            column: 0,
            cols: 4
        })
    );
    cx.simulate_click(
        at,
        Modifiers {
            platform: true,
            ..Modifiers::default()
        },
    );
    cx.run_until_parked();
    let opened = layout_of(&view, cx).active_tab().focused;
    assert_eq!(layout_of(&view, cx).file(opened), Some(file.as_path()));
    let cursor = cx.update(|_, cx| {
        view.read(cx).files.tabs[&opened]
            .editor()
            .unwrap()
            .read(cx)
            .cursor_position()
    });
    assert_eq!(cursor, gpui_component::input::Position::new(1, 2));
}

#[gpui::test]
fn wrapped_file_remote_guard_and_expired_negative_cache(cx: &mut TestAppContext) {
    let dir = TempDir::new("wrapped-cache");
    fs::create_dir_all(dir.0.join("src")).unwrap();
    let (view, cx) = open(cx, state_path("wrapped-cache"));
    let (pane, at) = show_wrapped_path(&view, cx, &dir.0);
    view.update(cx, |s, cx| {
        s.path_link_at(pane, at, None, cx);
    });
    cx.run_until_parked();
    assert!(cx.update(|_, cx| {
        view.read(cx)
            .files
            .link_lookup
            .as_ref()
            .unwrap()
            .found
            .is_none()
    }));
    fs::write(dir.0.join("src/long-name.rs"), "first\nsecond\n").unwrap();
    view.update(cx, |s, cx| {
        s.files.link_lookup.as_mut().unwrap().checked_at =
            std::time::Instant::now().checked_sub(Duration::from_secs(1));
        // 내부 캐시도 같은 유효기간으로 만료시킨다.
        s.files.link_cache.lock().unwrap().0 = std::time::Instant::now()
            .checked_sub(Duration::from_secs(1))
            .unwrap();
        s.path_link_at(pane, at, None, cx);
    });
    cx.run_until_parked();
    assert!(cx.update(|_, cx| {
        view.read(cx)
            .files
            .link_lookup
            .as_ref()
            .unwrap()
            .found
            .is_some()
    }));
    view.update(cx, |s, cx| {
        s.layout
            .set_host(pane, krill_app::hosts::HostId::Remote("other".into()));
        assert!(
            s.path_link_at(pane, at, Some(terminal_menu::FileAction::Open), cx)
                .is_none()
        );
        s.update_path_hover(pane, at, true, cx);
        assert!(s.path_hover(pane).is_none());
        s.open_terminal_file(
            dir.0.join("src/long-name.rs"),
            at,
            terminal_menu::FileAction::Open,
            cx,
        );
    });
    cx.run_until_parked();
    assert!(layout_of(&view, cx).file(pane).is_none());
}

/// Reviewed content is checked too, even when a writer preserves size and mtime.
#[gpui::test]
fn reload_comparison_rejects_different_content_with_identical_stamp(cx: &mut TestAppContext) {
    let dir = TempDir::new("reload-compare-content");
    let file = dir.0.join("file.txt");
    fs::write(&file, "initial").unwrap();
    let (view, cx) = open(cx, state_path("reload-compare-content"));
    let pane = open_file(&view, &file, cx);
    cx.simulate_input("draft ");
    fs::write(&file, "disk one").unwrap();
    let modified = fs::metadata(&file).unwrap().modified().unwrap();
    let stamp = editor::stamp(&file).unwrap();
    cx.update(|window, cx| view.update(cx, |shell, cx| shell.compare_file(pane, window, cx)));
    cx.run_until_parked();
    fs::write(&file, "disk two").unwrap();
    fs::File::options()
        .write(true)
        .open(&file)
        .unwrap()
        .set_times(fs::FileTimes::new().set_modified(modified))
        .unwrap();
    assert_eq!(editor::stamp(&file).unwrap(), stamp);
    cx.update(|window, cx| {
        view.update(cx, |shell, cx| {
            shell.apply_file_comparison(pane, window, cx);
        });
    });
    cx.run_until_parked();
    assert_eq!(editor_text(&view, pane, cx), "draft initial");
    assert_eq!(fs::read_to_string(file).unwrap(), "disk two");
    assert!(dirty(&view, pane, cx));
    view.read_with(cx, |shell, _| {
        assert!(shell.files.tabs[&pane].recovery.is_none());
    });
}

#[gpui::test]
fn restart_quit_stops_after_helper_failure_or_payload_replacement(cx: &mut TestAppContext) {
    use krill_app::updater::{RestartStatus, StagedUpdate, UpdateState};
    let staged = StagedUpdate {
        state_dir: "/test/restart-quit".into(),
        app_bundle: "/test/Krill.app".into(),
        from_version: "0.1.0".into(),
        to_version: "0.2.0".into(),
        manifest_url: "https://example.test/latest.json".into(),
        relaunched: false,
    };
    cx.update(|cx| {
        cx.set_global(UpdateState {
            staged: Some(staged.clone()),
            restart: RestartStatus::Starting,
            ..Default::default()
        });
        assert!(super::restart_quit_is_current(Some(&staged), cx));
        let mut failed = cx.global::<UpdateState>().clone();
        failed.restart = RestartStatus::Failed("helper exited".into());
        cx.set_global(failed);
        assert!(!super::restart_quit_is_current(Some(&staged), cx));
        assert!(
            super::restart_quit_is_current(None, cx),
            "ordinary quit remains available"
        );
        let mut next = staged.clone();
        next.to_version = "0.3.0".into();
        cx.set_global(UpdateState {
            staged: Some(next),
            restart: RestartStatus::Starting,
            ..Default::default()
        });
        assert!(!super::restart_quit_is_current(Some(&staged), cx));
    });
}

#[gpui::test]
fn restart_quit_does_not_exit_when_helper_fails_during_unsaved_prompt(cx: &mut TestAppContext) {
    use krill_app::updater::{RestartStatus, StagedUpdate, UpdateState};
    let dir = TempDir::new("restart-prompt-expired");
    let file = dir.0.join("a.txt");
    fs::write(&file, "saved").unwrap();
    let (view, cx) = open(cx, state_path("restart-prompt-expired"));
    let pane = open_file(&view, &file, cx);
    cx.simulate_input("draft ");
    let quitting = std::rc::Rc::new(std::cell::Cell::new(false));
    cx.update(|_, cx| {
        cx.set_global(QuitObserved(quitting.clone()));
        cx.set_global(UpdateState {
            staged: Some(StagedUpdate {
                state_dir: dir.0.clone(),
                app_bundle: dir.0.join("Krill.app"),
                from_version: "0.1.0".into(),
                to_version: "0.2.0".into(),
                manifest_url: "https://example.test/latest.json".into(),
                relaunched: false,
            }),
            restart: RestartStatus::Starting,
            ..Default::default()
        });
        quit_app(cx);
    });
    cx.run_until_parked();
    assert!(cx.has_pending_prompt());
    cx.update(|_, cx| {
        let mut state = cx.global::<UpdateState>().clone();
        state.restart = RestartStatus::Failed("helper timed out".into());
        cx.set_global(state);
    });
    cx.simulate_prompt_answer(etext::DISCARD.get());
    cx.run_until_parked();
    assert!(!quitting.get());
    view.read_with(cx, |shell, _| {
        assert!(!shell.files.tabs.contains_key(&pane));
    });
    assert!(cx.update(|_, cx| !cx.windows().is_empty()));
}

#[gpui::test]
fn session_tab_keeps_focus_after_viewing_file_and_closing_or_reloading(cx: &mut TestAppContext) {
    let dir = TempDir::new("session-focus-after-file");
    let md = dir.0.join("REPORT.md");
    fs::write(&md, "# Report\n\ninitial\n").unwrap();

    let (view, cx) = open(cx, state_path("session-focus-after-file"));
    let session_pane = view.read_with(cx, |shell, _| shell.layout.focused().unwrap());
    assert!(
        cx.update(|window, cx| view.read(cx).focus.is_focused(window)),
        "초기 세션 탭은 Shell 포커스를 가져야 한다"
    );

    // 1. 마크다운 파일(`REPORT.md`)을 조회한 뒤 편집 모드로 전환해 에디터가 포커스를 가져가게 한다.
    let file_pane = open_file(&view, &md, cx);
    assert_eq!(
        view.read_with(cx, |s, _| s.is_file_preview(file_pane)),
        Some(true)
    );
    let toggle = cx.debug_bounds("file-preview-toggle").unwrap().center();
    cx.simulate_click(toggle, Modifiers::default());
    cx.run_until_parked();
    assert!(
        cx.update(|window, cx| view.read(cx).is_editor_focused(window, cx)),
        "편집 모드 전환 후에는 에디터가 포커스를 가져야 한다"
    );

    // 2. 마우스로 세션 탭(0번)을 클릭해 돌아온 뒤 비활성 파일 탭(1번)의 닫기 버튼(`close-tab-1`)을 눌러 닫아도
    // 세션 탭의 키보드 포커스(`shell.focus`)가 끊기지 않아야 한다.
    let tab0 = cx.debug_bounds("session-tab-0").unwrap().center();
    cx.simulate_click(tab0, Modifiers::default());
    cx.run_until_parked();
    assert_eq!(
        view.read_with(cx, |shell, _| shell.layout.focused()),
        Some(session_pane)
    );
    assert!(
        cx.update(|window, cx| view.read(cx).focus.is_focused(window)),
        "세션 탭을 클릭해 돌아오면 Shell 포커스가 복원되어야 한다"
    );

    let tab1 = cx.debug_bounds("session-tab-1").unwrap().center();
    cx.simulate_mouse_move(tab1, None, Modifiers::default());
    cx.run_until_parked();
    let close_file_tab = cx.debug_bounds("close-tab-1").unwrap().center();
    cx.simulate_click(close_file_tab, Modifiers::default());
    cx.run_until_parked();
    assert!(
        cx.update(|window, cx| view.read(cx).focus.is_focused(window)),
        "비활성 파일 탭의 닫기 버튼을 눌러 닫은 뒤에도 세션 포커스가 유지되어야 한다"
    );

    // 3. 다시 파일을 열어 둔 상태에서 세션 탭으로 돌아오고(검색 바가 열려 있더라도),
    // 백그라운드 감시가 디스크 변경을 자동 리로드할 때 숨겨진 에디터가 세션 포커스를 뺏지 않아야 한다.
    let _file_pane2 = open_file(&view, &md, cx);
    let toggle = cx.debug_bounds("file-preview-toggle").unwrap().center();
    cx.simulate_click(toggle, Modifiers::default());
    cx.run_until_parked();

    cx.update(|window, cx| {
        view.update(cx, |shell, cx| {
            // 검색 바를 열어 둔 채 세션 탭으로 복귀
            let bar = cx.new(|cx| krill_app::search::Bar::new(window, cx));
            let sub = cx.subscribe_in(&bar, window, |_, _, _: &krill_app::search::Event, _, _| {});
            shell.search = Some((bar, sub));
            shell.change(true, cx, |l| l.activate(0));
        });
    });
    cx.run_until_parked();
    assert!(
        cx.update(|window, cx| view.read(cx).focus.is_focused(window)),
        "검색 바가 열려 있어도 파일 탭에서 세션 탭으로 돌아오면 Shell 포커스가 복원되어야 한다"
    );

    fs::write(&md, "# Report\n\nupdated by agent in background\n").unwrap();
    tick(cx);
    assert!(
        cx.update(|window, cx| view.read(cx).focus.is_focused(window)),
        "백그라운드 파일 자동 리로드가 실행 중인 세션 탭의 포커스를 뺏으면 안 된다"
    );
}

#[gpui::test]
fn remote_terminal_image_click_and_hover_open_staged_image_tab(cx: &mut TestAppContext) {
    use gpui::{Modifiers, point};
    use krill_app::hosts::{HostId, HostTarget};
    use krill_proto::control::{FEAT_CONTROL, FEAT_REMOTE_FS, Reply, Request};
    use krill_proto::ipc::{ClientHello, ServerHello};
    use krill_proto::os::net::UnixListener;
    use krill_proto::remote_fs::{
        RemoteFsReply, RemoteFsRequest, WireReadChunk, WireStamp, WireStat,
    };
    use krill_proto::screen::{Cell, Msg as _, Op, Revision, Seq, encode_update};
    use std::io::Write as _;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    let dir = TempDir::new("remote-image-open");
    let sock_path = dir.0.join("remote-d.sock");
    let listener = UnixListener::bind(&sock_path).unwrap();
    let remote_img = PathBuf::from("/Users/remote/visual/threads/krill-threads-card.png");
    let remote_img_wire = remote_img.as_os_str().as_bytes().to_vec();
    let mut png_bytes = b"\x89PNG\r\n\x1a\n\0\0\0\x0dIHDR".to_vec();
    png_bytes.extend_from_slice(&[0, 0, 0, 1, 0, 0, 0, 1, 8, 6, 0, 0, 0]);
    let probe_calls = Arc::new(AtomicUsize::new(0));
    let probe_calls_bg = probe_calls.clone();
    let stop = Arc::new(AtomicBool::new(false));
    let stop_bg = stop.clone();

    let daemon = std::thread::spawn(move || {
        while let Ok((mut stream, _)) = listener.accept() {
            if stop_bg.load(Ordering::SeqCst) {
                break;
            }
            let mut writer = stream.try_clone().unwrap();
            let server = ServerHello {
                proto_min: ipc::PROTO_MIN,
                proto_max: ipc::PROTO_MAX,
                features: FEAT_CONTROL | FEAT_REMOTE_FS,
                build: "test".to_owned(),
                instance_id: 1,
            };
            if writer.write_all(&server.encode().unwrap()).is_err() {
                continue;
            }
            let Some((kind, body)) = ipc::read_frame(&mut stream).ok().flatten() else {
                continue;
            };
            let _ = ClientHello::decode(kind, &body).unwrap();
            let Some((kind, payload)) = ipc::read_frame(&mut stream).ok().flatten() else {
                continue;
            };
            let Ok(req) = Request::decode(kind, &payload) else {
                continue;
            };
            let reply = match req {
                Request::RemoteFs(RemoteFsRequest::ProbePaths { paths, .. }) => {
                    probe_calls_bg.fetch_add(1, Ordering::SeqCst);
                    let resolved = paths
                        .into_iter()
                        .map(|p| (p == remote_img_wire).then(|| remote_img_wire.clone()))
                        .collect();
                    Reply::RemoteFs(RemoteFsReply::ProbedPaths(resolved))
                }
                Request::RemoteFs(RemoteFsRequest::ReadFile { .. }) => {
                    Reply::RemoteFs(RemoteFsReply::FileChunk(Ok(WireReadChunk {
                        stat: WireStat {
                            exists: true,
                            is_file: true,
                            is_dir: false,
                            is_symlink: false,
                            stamp: Some(WireStamp {
                                modified_unix_ns: Some(1_700_000_000_000_000_000),
                                len: u64::try_from(png_bytes.len()).unwrap(),
                            }),
                        },
                        bytes: png_bytes.clone(),
                        eof: true,
                    })))
                }
                Request::RemoteFs(RemoteFsRequest::Stat { .. }) => {
                    Reply::RemoteFs(RemoteFsReply::Stat(Ok(WireStat {
                        exists: true,
                        is_file: true,
                        is_dir: false,
                        is_symlink: false,
                        stamp: Some(WireStamp {
                            modified_unix_ns: Some(1_700_000_000_000_000_000),
                            len: u64::try_from(png_bytes.len()).unwrap(),
                        }),
                    })))
                }
                _ => continue,
            };
            stream.write_all(&reply.encode().unwrap()).ok();
        }
    });

    let (view, cx) = open(cx, state_path("remote-image-open"));
    let pane = layout_of(&view, cx).active_tab().focused;
    let host = HostId::Remote("macmini".into());
    let (ours, _theirs) = UnixStream::pair().unwrap();
    let line_text = format!("  {}", remote_img.display());
    view.update(cx, |s, cx| {
        s.host_targets.insert(
            host.clone(),
            HostTarget::remote("macmini", sock_path.clone()),
        );
        s.layout.set_host(pane, host.clone());
        s.attach_test(pane, krill_proto::SessionId(860), ours, cx);
        let cells: Vec<_> = line_text
            .chars()
            .map(|ch| Cell {
                ch,
                ..Cell::default()
            })
            .collect();
        let ops = vec![
            Op::Reset { cols: 80, lines: 4 },
            Op::Line { line: 1, cells },
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

    // 1. ⌘ 없이 마우스만 올려도 원격 ProbePaths를 거쳐 밑줄(및 포인터 커서)이 뜨고,
    // 같은 경로의 옆 열로 움직여도 재조회 없이 밑줄이 유지된다.
    cx.simulate_mouse_move(at(5.), None, Modifiers::default());
    cx.run_until_parked();
    let expected_hl = Some(Highlight {
        line: 1,
        column: 2,
        cols: u16::try_from(remote_img.to_str().unwrap().len()).unwrap(),
    });
    assert_eq!(
        cx.update(|_, cx| view.read(cx).path_hover(pane)),
        expected_hl
    );
    let calls_after_hover = probe_calls.load(Ordering::SeqCst);
    assert_eq!(calls_after_hover, 1);

    cx.simulate_mouse_move(at(12.), None, Modifiers::default());
    cx.run_until_parked();
    assert_eq!(
        cx.update(|_, cx| view.read(cx).path_hover(pane)),
        expected_hl
    );
    assert_eq!(
        probe_calls.load(Ordering::SeqCst),
        calls_after_hover,
        "같은 링크 범위 안 열 이동은 원격 ProbePaths를 다시 호출하지 않는다"
    );
    cx.simulate_mouse_move(at(12.), None, cmd);
    cx.run_until_parked();
    assert_eq!(
        cx.update(|_, cx| view.read(cx).path_hover(pane)),
        expected_hl
    );

    // 2. 원격 터미널에서 이미지를 일반 클릭(⌘ 없이)해도 원격 호스트의 이미지 탭으로 열린다.
    cx.simulate_click(at(12.), Modifiers::default());
    cx.run_until_parked();
    let opened = layout_of(&view, cx).active_tab().focused;
    assert_eq!(
        layout_of(&view, cx).file(opened),
        Some(remote_img.as_path())
    );
    assert_eq!(layout_of(&view, cx).host(opened), &host);
    assert!(cx.update(|_, cx| matches!(view.read(cx).files.tabs[&opened].body, Body::Image(_))));

    stop.store(true, Ordering::SeqCst);
    drop(UnixStream::connect(&sock_path));
    daemon.join().ok();
}
