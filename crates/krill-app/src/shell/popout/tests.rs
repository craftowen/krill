use std::{os::unix::net::UnixStream, sync::Arc, time::Duration};

use gpui::{TestAppContext, VisualTestContext};
use krill_proto::{control::AgentStatus, input::Input, ipc, screen::Msg as _};

use super::*;

#[test]
fn status_indicators_expose_status_role_and_label() {
    let p = &krill_app::ui::DARK;
    let el = daemon_status_dot("daemon up".to_owned(), p.success);
    let mut node = gpui::accesskit::Node::new(gpui::Role::Unknown);
    gpui::Element::write_a11y_info(&el, &mut node);
    assert_eq!(gpui::Element::a11y_role(&el), Some(gpui::Role::Status));
    assert_eq!(node.label(), Some("daemon up"));

    let el = usage_status(p, 3, 1);
    let mut node = gpui::accesskit::Node::new(gpui::Role::Unknown);
    gpui::Element::write_a11y_info(&el, &mut node);
    assert_eq!(gpui::Element::a11y_role(&el), Some(gpui::Role::Status));
    assert_eq!(node.label().map(str::to_owned), Some(counts(3, 1)));
}

#[gpui::test]
fn provider_popup_exposes_account_status_and_updated_row(cx: &mut TestAppContext) {
    use krill_proto::usage::{AccountInfo, AccountStatus, AccountStatusInfo, Snapshot};
    let (shell, handle) = open(cx);
    cx.run_until_parked();
    shell.update(cx, |s, cx| {
        s.accounts_snapshot.accounts = vec![AccountInfo {
            id: "claude-status-test".into(),
            provider: ProviderKind::Claude,
            label: "Test".into(),
            email: None,
            profile_dir: "/tmp/status-test".into(),
            active: true,
            host_default: false,
            stale: false,
            usage: Snapshot::default(),
        }];
        s.accounts_snapshot.statuses = vec![AccountStatusInfo {
            account_id: "claude-status-test".into(),
            status: AccountStatus::RateLimited,
            updated_at: Some(krill_app::accounts::display::now()),
            detail: None,
        }];
        cx.notify();
    });
    cx.run_until_parked();
    let vcx = &mut VisualTestContext::from_window(handle, cx);
    let pill = vcx.debug_bounds("status-provider-pill-claude").unwrap();
    vcx.simulate_click(pill.center(), gpui::Modifiers::default());
    vcx.run_until_parked();
    // OWE-851d: 상태 배지는 계정 행에만 그린다(헤더 카드 중복 제거).
    assert!(vcx.debug_bounds("status-provider-status-claude").is_none());
    assert!(
        vcx.debug_bounds("status-provider-account-claude-claude-status-test-status")
            .is_some()
    );
}

/// 하-4: 상태바를 우클릭하면 상태바 동작 메뉴가 열린다.
#[gpui::test]
fn status_bar_right_click_opens_menu(cx: &mut TestAppContext) {
    let (_shell, handle) = open(cx);
    cx.run_until_parked();
    let vcx = &mut VisualTestContext::from_window(handle, cx);
    assert!(vcx.debug_bounds("status-bar-menu-refresh").is_none());
    let bar = vcx.debug_bounds("status-bar").unwrap();
    vcx.simulate_mouse_down(
        bar.center(),
        gpui::MouseButton::Right,
        gpui::Modifiers::default(),
    );
    vcx.simulate_mouse_up(
        bar.center(),
        gpui::MouseButton::Right,
        gpui::Modifiers::default(),
    );
    vcx.run_until_parked();
    assert!(vcx.debug_bounds("status-bar-menu-refresh").is_some());
}

fn open(cx: &mut TestAppContext) -> (Entity<Shell>, gpui::AnyWindowHandle) {
    cx.update(|cx| {
        gpui_component::init(cx);
        super::super::bind_keys(cx);
    });
    let handle = cx.add_window(|window, cx| {
        let shell = Shell::new(None, cx);
        window.focus(shell.focus_handle(), cx);
        shell
    });
    let shell = handle.update(cx, |_, _, cx| cx.entity()).unwrap();
    (shell, handle.into())
}

#[gpui::test]
fn linked_account_refresh_keeps_daemon_snapshot_on_ipc_failure(cx: &mut TestAppContext) {
    use krill_proto::usage::{AccountStatus, AccountStatusInfo, AccountsSnapshot, UsageRequest};

    let (shell, _) = open(cx);
    let root = std::env::temp_dir().join(format!("acct-offline-{}", std::process::id()));
    let snapshot = AccountsSnapshot {
        statuses: vec![AccountStatusInfo {
            account_id: "daemon-account".to_owned(),
            status: AccountStatus::Ok,
            updated_at: Some(1),
            detail: None,
        }],
        ..AccountsSnapshot::default()
    };
    shell.update(cx, |shell, cx| {
        shell.paths = Some(krill_proto::daemon::Paths {
            socket: root.join("missing.sock"),
            holder: root.join("missing-holder"),
            krilld: root.join("missing-daemon"),
        });
        shell.link = Link::Up;
        shell.accounts_snapshot = snapshot.clone();
        shell.notice = None;
        shell.refresh_accounts(UsageRequest::ListAccounts, cx);
    });
    cx.run_until_parked();
    cx.executor().advance_clock(Duration::from_secs(2));
    cx.run_until_parked();
    let (actual, notice) =
        shell.read_with(cx, |s, _| (s.accounts_snapshot.clone(), s.notice.clone()));
    assert_eq!(
        actual, snapshot,
        "연결 중 IPC 실패를 로컬 스냅샷으로 숨기지 않는다"
    );
    assert!(notice.is_some(), "명시적 새로고침 실패를 알린다");
}

/// OWE-652: 상태바 '세션 대시보드' 버튼은 대시보드 창을 열고, 두 번 눌러도 창은 하나다.
#[gpui::test]
fn status_dashboard_button_opens_singleton_window(cx: &mut TestAppContext) {
    let (_main, main_window) = open(cx);
    cx.run_until_parked();
    let mut visual = VisualTestContext::from_window(main_window, cx);
    let bounds = visual
        .debug_bounds("dashboard-popout")
        .expect("dashboard button");
    visual.simulate_click(bounds.center(), gpui::Modifiers::default());
    cx.run_until_parked();
    assert_eq!(cx.windows().len(), 2, "dashboard click opens a window");
    cx.read(|cx| assert!(existing::<Dashboard>(cx).is_some()));
    let bounds = visual
        .debug_bounds("dashboard-popout")
        .expect("dashboard button");
    visual.simulate_click(bounds.center(), gpui::Modifiers::default());
    cx.run_until_parked();
    assert_eq!(
        cx.windows().len(),
        2,
        "second click focuses, not duplicates"
    );
}

/// OWE-652: 상태바 '세션 히스토리' 토글은 팝오버를 열고, 바깥 클릭은 닫는다.
#[gpui::test]
fn status_history_toggle_opens_and_outside_click_closes(cx: &mut TestAppContext) {
    let (main, main_window) = open(cx);
    cx.run_until_parked();
    let mut visual = VisualTestContext::from_window(main_window, cx);
    let toggle = visual
        .debug_bounds("agent-history-toggle")
        .expect("history toggle");
    visual.simulate_click(toggle.center(), gpui::Modifiers::default());
    cx.run_until_parked();
    let is_open =
        |cx: &mut TestAppContext| cx.update(|cx| main.read(cx).history.read(cx).is_open());
    assert!(is_open(cx), "history popover opens");
    // 열린 상태에서 헤더를 다시 누르면 닫힌 채로 둔다(바깥 닫기와 같은 제스처).
    visual.simulate_click(toggle.center(), gpui::Modifiers::default());
    cx.run_until_parked();
    assert!(!is_open(cx), "header click closes history popover");
    // 닫힌 상태에서 헤더를 누르면 다시 열린다.
    visual.simulate_click(toggle.center(), gpui::Modifiers::default());
    cx.run_until_parked();
    assert!(is_open(cx), "history popover reopens");
    // 팝오버(오른쪽 위 상태바)와 헤더를 피한 왼쪽 아래를 누르면 닫힌다.
    let size = visual.update(|window, _| window.viewport_size());
    assert!(
        size.width > gpui::px(560.) && size.height > gpui::px(200.),
        "window must fit popover + away point, got {size:?}"
    );
    let away = gpui::point(gpui::px(10.), size.height - gpui::px(10.));
    visual.simulate_click(away, gpui::Modifiers::default());
    cx.run_until_parked();
    assert!(!is_open(cx), "outside click closes history popover");
}

/// OWE-652: 오케스트레이션 빈 상태 조건 — Run·Task가 모두 없을 때만 안내한다.
#[test]
fn orch_empty_only_when_no_runs_and_no_tasks() {
    use krill_proto::orchestration::OrchestrationStore;
    assert!(!orch_has_content(&OrchestrationStore::default()));
}

/// AS-21: 플로팅 창은 링크를 직접 대입하지 않고 `set_link`으로 연결한다.
/// 데몬이 없어도 "연결됨" 경로(에이전트·계정 조회 시도)를 타 실패를 배너로 알린다.
#[gpui::test]
fn floating_window_connects_through_set_link(cx: &mut TestAppContext) {
    let (_main, _main_window) = open(cx);
    let dir = std::env::temp_dir().join(format!("mk-float-link-{}", std::process::id()));
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(&dir).unwrap();
    let paths = krill_proto::daemon::Paths {
        socket: dir.join("krilld.sock"),
        holder: dir.join("krill-holder"),
        krilld: dir.join("krilld"),
    };
    cx.update(|cx| {
        let theme = ui::terminal_theme(cx);
        open_floating(SessionId(99), Some(paths.into()), theme, cx).unwrap();
    });
    cx.run_until_parked();
    cx.executor().advance_clock(Duration::from_secs(2));
    cx.run_until_parked();
    let floating = cx.read(|cx| existing::<Floating>(cx).unwrap());
    let child = floating.update(cx, |f, _, _| f.shell.clone()).unwrap();
    let (link, notice) = child.read_with(cx, |s, _| (s.link.clone(), s.notice.clone()));
    assert_eq!(link, Link::Up, "플로팅 창도 데몬 상태를 공유한다");
    assert!(
        notice.is_some(),
        "연결 실패(에이전트·계정 조회 시도)를 배너로 알린다"
    );
    std::fs::remove_dir_all(&dir).ok();
}

/// AS-22: 질문 응답·Gate 승인은 백그라운드 저장 + 실패 배너 (UI 스레드 flock 대기 없음,
/// 실패를 조용히 버리지 않음).
#[gpui::test]
fn question_and_gate_resolve_off_ui_thread_and_report_errors(cx: &mut TestAppContext) {
    use krill_proto::orchestration::{MessageType, OrchestrationStore, SendParams};
    let (main, _main_window) = open(cx);
    let dir = std::env::temp_dir().join(format!("mk-orch-qa-{}", std::process::id()));
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(&dir).unwrap();
    let mut store = OrchestrationStore::default();
    let sent = store
        .send_message(&SendParams {
            from: "worker".to_owned(),
            to: "coordinator".to_owned(),
            msg_type: MessageType::Question,
            summary: "계속할까요?".to_owned(),
            body: String::new(),
            run_id: None,
            task_id: None,
            dispatch_id: None,
            outcome: None,
            reply_to: None,
        })
        .unwrap();
    let gate = store
        .create_gate("worker", "배포할까요?", vec![], None, None)
        .unwrap();
    store.save(&dir).unwrap();
    let qid = sent[0].id.clone();
    let gid = gate.id.clone();

    // 성공 경로: 저장되고 배너가 없다.
    cx.update(|cx| {
        main.update(cx, |_, cx| {
            Shell::answer_question(Some(dir.clone()), qid.clone(), "yes", cx);
            Shell::resolve_gate(Some(dir.clone()), gid.clone(), "approve", cx);
        });
    });
    cx.run_until_parked();
    cx.executor().advance_clock(Duration::from_secs(1));
    cx.run_until_parked();
    let reloaded = OrchestrationStore::load(&dir).unwrap();
    assert!(
        reloaded.messages.iter().any(|m| m.id == qid && m.acked),
        "질문 응답이 저장된다"
    );
    assert!(
        reloaded
            .gates
            .iter()
            .any(|g| g.id == gid && g.status != krill_proto::orchestration::GateStatus::Pending),
        "Gate 결정이 저장된다"
    );
    assert!(
        main.read_with(cx, |s, _| s.notice.clone()).is_none(),
        "성공하면 배너 없음"
    );

    // 실패 경로: 없는 Gate를 승인하면 배너로 알린다(버리지 않음).
    cx.update(|cx| {
        main.update(cx, |_, cx| {
            Shell::resolve_gate(Some(dir.clone()), "gate-missing".to_owned(), "approve", cx);
        });
    });
    cx.run_until_parked();
    cx.executor().advance_clock(Duration::from_secs(1));
    cx.run_until_parked();
    assert!(
        main.read_with(cx, |s, _| s.notice.clone())
            .is_some_and(|n| n.contains("승인 처리 실패")),
        "실패를 배너로 알린다"
    );
    std::fs::remove_dir_all(&dir).ok();
}

#[gpui::test]
fn floating_keys_ime_close_and_singleton_preserve_main_session(cx: &mut TestAppContext) {
    let (main, main_window) = open(cx);
    let (ours, mut main_peer) = UnixStream::pair().unwrap();
    cx.update(|cx| {
        main.update(cx, |s, cx| {
            let pane = s.layout.active_tab().focused;
            s.layout.set_session(pane, SessionId(41));
            s.attach_test(pane, SessionId(41), ours, cx);
        });
    });
    cx.simulate_keystrokes(main_window, "alt-cmd-a");
    cx.run_until_parked();
    let floating = cx.read(|cx| existing::<Floating>(cx).unwrap());
    let child = floating.update(cx, |f, _, _| f.shell.clone()).unwrap();
    let (ours, mut peer) = UnixStream::pair().unwrap();
    cx.update(|cx| {
        child.update(cx, |s, cx| {
            assert!(s.path.is_none());
            assert!(s.sidebar_collapsed && s.panel_collapsed);
            let pane = s.layout.active_tab().focused;
            assert_eq!(s.layout.session(pane), Some(SessionId(41)));
            s.attach_test(pane, SessionId(41), ours, cx);
        });
    });
    cx.run_until_parked();
    cx.simulate_input(floating.into(), "한글🙂");
    peer.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
    let mut input = Vec::new();
    while input.len() < "한글🙂".len() {
        let (k, body) = ipc::read_frame(&mut peer).unwrap().unwrap();
        input.extend(Input::decode(k, &body).unwrap().bytes);
    }
    assert_eq!(input, "한글🙂".as_bytes());

    // 플로팅 창 자신의 단축키로 재요청해도 동일 엔티티 중첩 update/새 창이 없다.
    cx.simulate_keystrokes(floating.into(), "alt-cmd-a");
    cx.run_until_parked();
    assert_eq!(cx.windows().len(), 2);
    cx.simulate_keystrokes(floating.into(), "cmd-w");
    cx.run_until_parked();
    drop(child);
    drop(peer);
    cx.update(|_| {});
    cx.run_until_parked();
    assert_eq!(cx.windows().len(), 1);
    // 창을 닫아도 세션 종료 요청을 보내지 않음은 메인 Live가 살아 입력이 전달되는 것으로 확인한다.
    // (gpui 테스트에서 창 제거 뒤 Entity drop 시점은 비결정적이라 소켓 EOF 단언은 하지 않는다.)

    // 메인 창의 같은 세션 입력은 계속 전달된다.
    cx.simulate_keystrokes(main_window, "enter");
    main_peer
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    let (k, body) = ipc::read_frame(&mut main_peer).unwrap().unwrap();
    assert_eq!(Input::decode(k, &body).unwrap().bytes, b"\r");
    assert_eq!(
        cx.read(|cx| main
            .read(cx)
            .layout
            .session(main.read(cx).layout.active_tab().focused)),
        Some(SessionId(41))
    );

    cx.simulate_keystrokes(main_window, "alt-cmd-a");
    cx.run_until_parked();
    assert_eq!(cx.windows().len(), 2, "닫은 뒤 재열기");
}

struct Fixed;
impl sidebar::Source for Fixed {
    fn fetch(&self) -> std::io::Result<sidebar::Snapshot> {
        Ok(sidebar::Snapshot {
            sessions: [
                AgentState::Working,
                AgentState::Waiting,
                AgentState::Blocked,
                AgentState::Done,
                AgentState::Exited,
                AgentState::Unknown,
            ]
            .into_iter()
            .enumerate()
            .map(|(i, state)| sidebar::Session {
                id: SessionId(u64::try_from(i).unwrap()),
                status: AgentStatus {
                    state,
                    ..AgentStatus::UNKNOWN
                },
                worktree: None,
                cwd: None,
                account: None,
            })
            .collect(),
            ..Default::default()
        })
    }
}

#[gpui::test]
fn dashboard_lanes_live_updates_click_and_singleton(cx: &mut TestAppContext) {
    let (main, main_window) = open(cx);
    cx.simulate_keystrokes(main_window, "alt-cmd-shift-a");
    cx.run_until_parked();
    let dashboard = cx.read(|cx| existing::<Dashboard>(cx).unwrap());
    dashboard
        .update(cx, |d, _, cx| {
            d.sidebar
                .update(cx, |s, cx| s.connect_source(Arc::new(Fixed), cx));
        })
        .unwrap();
    cx.run_until_parked();
    let mut visual = VisualTestContext::from_window(dashboard.into(), cx);
    for selector in [
        "dashboard-session-0",
        "dashboard-session-1",
        "dashboard-session-2",
        "dashboard-session-3",
        "dashboard-session-4",
        "dashboard-session-5",
    ] {
        assert!(visual.debug_bounds(selector).is_some());
    }
    assert_eq!(
        [
            lane(AgentState::Working),
            lane(AgentState::Waiting),
            lane(AgentState::Blocked),
            lane(AgentState::Done),
            lane(AgentState::Exited),
            lane(AgentState::Unknown)
        ],
        [0, 1, 1, 2, 2, 3]
    );
    cx.update(|cx| main.update(cx, super::super::Shell::dashboard));
    cx.run_until_parked();
    assert_eq!(cx.windows().len(), 2);
    let card = visual.debug_bounds("dashboard-session-2").unwrap();
    visual.simulate_click(card.center(), gpui::Modifiers::default());
    cx.run_until_parked();
    // OWE-851: 카드 클릭은 별도 플로팅 창 대신 메인 창 해당 세션으로 포커스한다.
    assert_eq!(cx.windows().len(), 2);
    assert!(cx.read(|cx| existing::<Floating>(cx).is_none()));
    let focused = main.read_with(cx, |s, _| {
        s.layout.focused().and_then(|p| s.layout.session(p))
    });
    assert_eq!(focused, Some(SessionId(2)));
    // 새 세션 선택은 같은 플로팅 창을 재사용한다.
    cx.update(|cx| main.update(cx, |s, cx| s.float_terminal(SessionId(5), cx)));
    cx.run_until_parked();
    assert_eq!(cx.windows().len(), 3);
    let floating = cx.read(|cx| existing::<Floating>(cx).unwrap());
    floating
        .update(cx, |f, _, cx| {
            let s = f.shell.read(cx);
            assert_eq!(
                s.layout.session(s.layout.active_tab().focused),
                Some(SessionId(5))
            );
        })
        .unwrap();
    cx.simulate_keystrokes(dashboard.into(), "cmd-w");
    cx.run_until_parked();
    assert_eq!(cx.windows().len(), 2);
}

/// OWE-861: 대시보드 팝아웃은 Esc로 닫히고, 실시간 미리보기가 있는 카드에는
/// '미리보기 없음'이 중복 출력되지 않는다.
#[gpui::test]
fn dashboard_esc_closes_and_preview_hides_no_preview(cx: &mut TestAppContext) {
    let (main, main_window) = open(cx);
    // 세션 2번 칸에 테스트 PTY를 붙여 미리보기가 생기게 한다.
    let (ours, _peer) = UnixStream::pair().unwrap();
    cx.update(|cx| {
        main.update(cx, |s, cx| {
            let pane = s.layout.active_tab().focused;
            s.layout.set_session(pane, SessionId(2));
            s.attach_test(pane, SessionId(2), ours, cx);
        });
    });
    cx.simulate_keystrokes(main_window, "alt-cmd-shift-a");
    cx.run_until_parked();
    let dashboard = cx.read(|cx| existing::<Dashboard>(cx).unwrap());
    dashboard
        .update(cx, |d, _, cx| {
            d.sidebar
                .update(cx, |s, cx| s.connect_source(Arc::new(Fixed), cx));
        })
        .unwrap();
    cx.run_until_parked();
    let mut visual = VisualTestContext::from_window(dashboard.into(), cx);
    // 미리보기가 있는 세션 2번 카드: PREVIEW만 있고 NO_PREVIEW는 없다.
    assert!(visual.debug_bounds("dashboard-preview-2").is_some());
    assert!(
        visual.debug_bounds("dashboard-no-preview-2").is_none(),
        "preview card must not show NO_PREVIEW"
    );
    // 미리보기가 없는 세션 0번 카드: NO_PREVIEW만 있고 PREVIEW는 없다.
    assert!(visual.debug_bounds("dashboard-no-preview-0").is_some());
    assert!(visual.debug_bounds("dashboard-preview-0").is_none());
    // Esc로 닫힌다.
    visual.simulate_keystrokes("escape");
    visual.run_until_parked();
    assert_eq!(cx.windows().len(), 1, "escape closes dashboard");
}

/// OWE-861: 플로팅 창에서 터미널 단축키(⌘F 찾기)가 Shell 키 문맥으로 동작한다.
#[gpui::test]
fn floating_find_shortcut_opens_search(cx: &mut TestAppContext) {
    let (main, main_window) = open(cx);
    let (ours, _peer) = UnixStream::pair().unwrap();
    cx.update(|cx| {
        main.update(cx, |s, cx| {
            let pane = s.layout.active_tab().focused;
            s.layout.set_session(pane, SessionId(61));
            s.attach_test(pane, SessionId(61), ours, cx);
        });
    });
    cx.simulate_keystrokes(main_window, "alt-cmd-a");
    cx.run_until_parked();
    let floating = cx.read(|cx| existing::<Floating>(cx).unwrap());
    let child = floating.update(cx, |f, _, _| f.shell.clone()).unwrap();
    // OWE-868: 검색 바는 라이브 세션이 있을 때만 열린다. 플로팅 셸은 실제 환경에서
    // 데몬 동기화로 라이브 세션이 붙으므로, 테스트에서도 같은 세션을 직접 붙여 준다.
    let (ours_floating, _peer_floating) = UnixStream::pair().unwrap();
    cx.update(|cx| {
        child.update(cx, |s, cx| {
            let pane = s.layout.active_tab().focused;
            s.layout.set_session(pane, SessionId(61));
            s.attach_test(pane, SessionId(61), ours_floating, cx);
        });
    });
    cx.run_until_parked();
    assert!(
        child.read_with(cx, |s, _| s.search.is_none()),
        "search starts closed"
    );
    cx.simulate_keystrokes(floating.into(), "cmd-f");
    cx.run_until_parked();
    assert!(
        child.read_with(cx, |s, _| s.search.is_some()),
        "cmd-f opens terminal search in floating window"
    );
}

/// OWE-851: 대시보드는 Esc로도 닫힌다.
#[gpui::test]
fn dashboard_escape_closes_window(cx: &mut TestAppContext) {
    let (_main, main_window) = open(cx);
    cx.simulate_keystrokes(main_window, "alt-cmd-shift-a");
    cx.run_until_parked();
    assert_eq!(cx.windows().len(), 2);
    let dashboard = cx.read(|cx| existing::<Dashboard>(cx).unwrap());
    cx.simulate_keystrokes(dashboard.into(), "escape");
    cx.run_until_parked();
    assert_eq!(cx.windows().len(), 1);
}

#[gpui::test]
fn dashboard_stable_poll_does_not_notify_and_changed_snapshot_refreshes(cx: &mut TestAppContext) {
    struct Empty;
    impl sidebar::Source for Empty {
        fn fetch(&self) -> std::io::Result<sidebar::Snapshot> {
            Ok(sidebar::Snapshot::default())
        }
    }
    let (main, main_window) = open(cx);
    cx.simulate_keystrokes(main_window, "alt-cmd-shift-a");
    cx.run_until_parked();
    let dashboard = cx.read(|cx| existing::<Dashboard>(cx).unwrap());
    let entity = dashboard
        .update(cx, |d, _, cx| {
            d.sidebar
                .update(cx, |s, cx| s.connect_source(Arc::new(Fixed), cx));
            cx.entity()
        })
        .unwrap();
    cx.run_until_parked();
    let notifications = std::rc::Rc::new(std::cell::Cell::new(0));
    let count = notifications.clone();
    let _subscription = cx.update(|cx| cx.observe(&entity, move |_, _| count.set(count.get() + 1)));
    for _ in 0..4 {
        cx.executor().advance_clock(Duration::from_millis(500));
        cx.run_until_parked();
    }
    assert_eq!(
        notifications.get(),
        0,
        "변화 없는 폴링은 redraw를 요청하지 않는다"
    );
    dashboard
        .update(cx, |d, _, cx| {
            d.sidebar
                .update(cx, |s, cx| s.connect_source(Arc::new(Empty), cx));
        })
        .unwrap();
    cx.executor().advance_clock(Duration::from_millis(500));
    cx.run_until_parked();
    assert!(notifications.get() > 0);
    assert!(entity.read_with(cx, |d, cx| d.sidebar.read(cx).jump_items().is_empty()));
    drop(main);
}

#[test]
fn preview_is_bounded_and_keeps_combining_characters() {
    let cell = krill_proto::screen::Cell {
        ch: 'ᄒ',
        zerowidth: vec!['ᅡ', 'ᆫ'],
        ..Default::default()
    };
    let screen = Screen {
        rows: vec![vec![cell; 50]; 50],
        ..Default::default()
    };
    let text = preview(&screen);
    assert_eq!(text.lines().count(), 3);
    assert!(text.lines().all(|line| line.chars().count() == 32 * 3));
}

/// OWE-633: 완료된 Run의 미해제 워커도 보인다. `Released`만 숨는다.
#[test]
fn unreleased_workers_includes_completed_runs_and_hides_released() {
    use krill_proto::orchestration::{Dispatch, DispatchStatus, OrchestrationStore};
    fn dispatch(id: &str, run: &str, status: DispatchStatus) -> Dispatch {
        Dispatch {
            id: id.to_owned(),
            run_id: run.to_owned(),
            task_id: None,
            worker_name: format!("worker-{id}"),
            agent: "claude".to_owned(),
            session_id: Some(7),
            cwd: "/tmp".to_owned(),
            prompt: "do it".to_owned(),
            status,
            summary: None,
            archived_output: None,
            started_at_ms: 1,
            ended_at_ms: None,
        }
    }
    let store = OrchestrationStore {
        dispatches: vec![
            dispatch("active", "run-new", DispatchStatus::Active),
            dispatch("done-old", "run-completed", DispatchStatus::Succeeded),
            dispatch("failed-old", "run-completed", DispatchStatus::Failed),
            dispatch("gone", "run-completed", DispatchStatus::Released),
        ],
        ..OrchestrationStore::default()
    };
    let shown: Vec<_> = unreleased_workers(&store)
        .iter()
        .map(|d| d.id.as_str())
        .collect();
    assert_eq!(shown, ["active", "done-old", "failed-old"]);
}

/// OWE-702: 데몬 연결 중에만 `ACCOUNTS_POLL`마다 계정을 다시 묻고, 링크가 끊기면 멈춘다.
/// 가짜 데몬 소켓은 연결 수만 센다(응답 없음 → 조용히 다음 바퀴).
#[gpui::test]
fn accounts_poll_runs_only_while_linked(cx: &mut TestAppContext) {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let (main, _window) = open(cx);
    let dir = std::env::temp_dir().join(format!("mk-acct-poll-{}", std::process::id()));
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(&dir).unwrap();
    let socket = dir.join("krilld.sock");
    let listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
    let hits = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&hits);
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            counter.fetch_add(1, Ordering::SeqCst);
            drop(stream);
        }
    });
    let paths = krill_proto::daemon::Paths {
        socket,
        holder: dir.join("krill-holder"),
        krilld: dir.join("krilld"),
    };
    let wait_for = |n: usize| {
        for _ in 0..200 {
            if hits.load(Ordering::SeqCst) >= n {
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        hits.load(Ordering::SeqCst)
    };
    main.update(cx, |s, cx| {
        s.paths = Some(paths);
        s.link = Link::Up;
        s.accounts_poll = Some(Shell::poll_accounts(cx));
    });
    cx.run_until_parked();
    assert_eq!(wait_for(1), 0, "타이머 전에는 묻지 않는다");
    cx.executor().advance_clock(ACCOUNTS_POLL);
    cx.run_until_parked();
    assert_eq!(wait_for(1), 1, "연결 중 한 바퀴");
    cx.executor().advance_clock(ACCOUNTS_POLL);
    cx.run_until_parked();
    assert_eq!(wait_for(2), 2, "연결 중 다음 바퀴");
    assert!(
        main.read_with(cx, |s, _| s.notice.clone()).is_none(),
        "주기 갱신 실패는 배너를 띄우지 않는다"
    );

    main.update(cx, |s, _| s.link = Link::Down);
    cx.executor().advance_clock(ACCOUNTS_POLL * 3);
    cx.run_until_parked();
    assert_eq!(wait_for(3), 2, "끊기면 묻지 않는다");
    std::fs::remove_dir_all(&dir).ok();
}

/// OWE-706: 설치 감지에서 빠진 프로바이더는 계정이 있어도 상태바 알약을 그리지 않는다.
#[gpui::test]
fn status_bar_hides_pills_of_not_installed_providers(cx: &mut TestAppContext) {
    use krill_proto::usage::{AccountInfo, Snapshot};
    let account = |provider: ProviderKind, id: &str| AccountInfo {
        id: id.into(),
        provider,
        label: id.into(),
        email: None,
        profile_dir: format!("/tmp/{id}"),
        active: true,
        host_default: false,
        stale: false,
        usage: Snapshot::default(),
    };
    let (shell, handle) = open(cx);
    cx.update(|cx| {
        cx.set_global(krill_app::accounts::installed::Installed(Some(vec![
            ProviderKind::Claude,
        ])));
    });
    shell.update(cx, |s, cx| {
        s.accounts_snapshot.accounts = vec![
            account(ProviderKind::Claude, "claude-a"),
            account(ProviderKind::Codex, "codex-a"),
        ];
        cx.notify();
    });
    cx.run_until_parked();
    let vcx = &mut VisualTestContext::from_window(handle, cx);
    assert!(vcx.debug_bounds("status-provider-pill-claude").is_some());
    assert!(vcx.debug_bounds("status-provider-pill-codex").is_none());
}

/// MK-156: 로컬 HTTP 테스트 서버에서 새 버전을 스테이징하면 플로팅 `UpdateCard`와 상태바 배지가 표시되고,
/// `Esc`·`[-]` 축소 버튼·상태바 배지 클릭으로 접기·펼치기가 토글되며, `[재시작하여 업데이트]` 클릭 시 relauncher가 기동된다.
#[gpui::test]
fn update_card_and_status_bar_badge_toggle_collapse_and_expand(cx: &mut TestAppContext) {
    use krill_app::updater::{CheckStatus, UpdateState, stage_update_for_bundle};
    use std::fs;
    use std::io::{Read as _, Write as _};
    use std::net::TcpListener;
    use std::os::unix::fs::PermissionsExt as _;
    use std::process::Command;

    let tmp = std::env::temp_dir().join(format!("mk156-update-card-{}", std::process::id()));
    fs::remove_dir_all(&tmp).ok();
    let state = tmp.join("state");
    fs::create_dir_all(&state).unwrap();

    let repo_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap();
    let script = repo_root.join("packaging/macos/ed25519_manifest.py");
    let key_file = tmp.join("release.ed25519.key");
    let keygen = Command::new("/usr/bin/python3")
        .arg(&script)
        .args(["keygen", "--key-file"])
        .arg(&key_file)
        .output()
        .unwrap();
    assert!(keygen.status.success());
    let pub_hex = String::from_utf8(keygen.stdout).unwrap().trim().to_owned();

    let make_bundle = |bundle: &std::path::Path, ver: &str| {
        let macos = bundle.join("Contents/MacOS");
        fs::create_dir_all(&macos).unwrap();
        fs::write(
            bundle.join("Contents/Info.plist"),
            format!(
                r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>CFBundleIdentifier</key><string>dev.krill.app</string>
<key>CFBundleExecutable</key><string>krill-app</string>
<key>CFBundleShortVersionString</key><string>{ver}</string>
</dict></plist>"#
            ),
        )
        .unwrap();
        let exe = macos.join("krill-app");
        fs::write(&exe, "#!/bin/sh\nexit 0\n").unwrap();
        fs::set_permissions(&exe, fs::Permissions::from_mode(0o755)).unwrap();
        assert!(
            Command::new("/usr/bin/codesign")
                .args(["--force", "-s", "-"])
                .arg(&exe)
                .status()
                .unwrap()
                .success()
        );
        assert!(
            Command::new("/usr/bin/codesign")
                .args(["--force", "-s", "-"])
                .arg(bundle)
                .status()
                .unwrap()
                .success()
        );
    };

    let app = tmp.join("Krill.app");
    make_bundle(&app, "0.1.1");
    let v2_bundle = tmp.join("v2/Krill.app");
    make_bundle(&v2_bundle, "0.2.0");
    let zip_path = tmp.join("Krill-0.2.0.zip");
    assert!(
        Command::new("/usr/bin/ditto")
            .args(["-c", "-k", "--keepParent"])
            .arg(&v2_bundle)
            .arg(&zip_path)
            .status()
            .unwrap()
            .success()
    );
    let zip_bytes = fs::read(&zip_path).unwrap();
    let zip_sha = krill_app::updater::ed25519::sha256_hex(&zip_bytes);

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base_url = format!("http://127.0.0.1:{}", listener.local_addr().unwrap().port());
    let manifest_url = format!("{base_url}/latest.json");
    let zip_url = format!("{base_url}/Krill-0.2.0.zip");
    let manifest_file = tmp.join("latest.json");
    assert!(
        Command::new("/usr/bin/python3")
            .arg(&script)
            .args([
                "sign",
                "--key-file",
                &key_file.to_string_lossy(),
                "--version",
                "0.2.0",
                "--url",
                &zip_url,
                "--sha256",
                &zip_sha,
                "--output",
                &manifest_file.to_string_lossy(),
            ])
            .status()
            .unwrap()
            .success()
    );
    let manifest_bytes = fs::read(&manifest_file).unwrap();
    fs::write(
        state.join(krill_app::updater::CONFIG_FILE),
        format!("url = \"{manifest_url}\"\npublic_key = \"{pub_hex}\"\n"),
    )
    .unwrap();

    let server = std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let mut stream = stream;
            let mut buf = [0u8; 4096];
            let n = stream.read(&mut buf).unwrap_or(0);
            let req = String::from_utf8_lossy(&buf[..n]);
            let path = req
                .lines()
                .next()
                .and_then(|l| l.split_whitespace().nth(1))
                .unwrap_or("/");
            let body: &[u8] = match path {
                "/latest.json" => &manifest_bytes,
                "/Krill-0.2.0.zip" => &zip_bytes,
                "/stop" => break,
                _ => b"not found",
            };
            let header = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            stream.write_all(header.as_bytes()).ok();
            stream.write_all(body).ok();
        }
    });

    let mut staged = stage_update_for_bundle(Some(&manifest_url), &app, &state)
        .unwrap()
        .expect("staged from local HTTP server");
    // GitHub 릴리스 URL일 때 릴리스 노트 버튼도 함께 검증한다.
    staged.manifest_url =
        "https://github.com/craftowen/krill/releases/latest/download/latest.json".to_owned();

    let (shell, handle) = open(cx);
    cx.update(|cx| {
        cx.set_global(UpdateState {
            status: CheckStatus::Ready(staged.clone()),
            staged: Some(staged),
            card_collapsed: false,
        });
    });
    shell.update(cx, |_, cx| cx.notify());
    cx.run_until_parked();

    let vcx = &mut VisualTestContext::from_window(handle, cx);
    assert!(vcx.debug_bounds("update-card").is_some());
    assert!(vcx.debug_bounds("update-card-minimize").is_some());
    assert!(vcx.debug_bounds("update-card-restart").is_some());
    assert!(vcx.debug_bounds("update-card-release-notes").is_some());
    assert!(vcx.debug_bounds("status-update-ready").is_some());

    // Esc 키로 플로팅 카드를 상태바 배지로 축소할 수 있어야 한다(MK-156).
    vcx.simulate_keystrokes("escape");
    vcx.run_until_parked();
    assert!(
        vcx.debug_bounds("update-card").is_none(),
        "Escape key collapses floating card to status bar"
    );
    assert!(vcx.debug_bounds("status-update-ready").is_some());

    // 상태바 배지를 클릭하면 다시 펼쳐진다.
    let badge = vcx.debug_bounds("status-update-ready").unwrap();
    vcx.simulate_click(badge.center(), gpui::Modifiers::default());
    vcx.run_until_parked();
    assert!(
        vcx.debug_bounds("update-card").is_some(),
        "status bar badge re-expands floating card"
    );

    // [-] 축소 버튼 클릭 시 상태바 배지로 접힌다.
    let minimize = vcx.debug_bounds("update-card-minimize").unwrap();
    vcx.simulate_click(minimize.center(), gpui::Modifiers::default());
    vcx.run_until_parked();
    assert!(
        vcx.debug_bounds("update-card").is_none(),
        "minimize button collapses floating card"
    );
    assert!(
        vcx.debug_bounds("status-update-ready").is_some(),
        "status bar badge remains visible when card is collapsed"
    );

    // 다시 펼친 뒤 [재시작하여 업데이트] 클릭 시 relauncher가 준비·기동된다.
    let badge = vcx.debug_bounds("status-update-ready").unwrap();
    vcx.simulate_click(badge.center(), gpui::Modifiers::default());
    vcx.run_until_parked();
    let restart = vcx.debug_bounds("update-card-restart").unwrap();
    vcx.simulate_click(restart.center(), gpui::Modifiers::default());
    vcx.run_until_parked();
    assert!(
        state.join("update/relauncher").is_file(),
        "clicking restart button stages and spawns detached relauncher"
    );

    krill_app::updater::fetch_bytes(&format!("{base_url}/stop"), 64).ok();
    server.join().ok();
    fs::remove_dir_all(&tmp).ok();
}

/// OWE-788: 상태바 우측 항목은 아이콘 중심으로 축약되어 그려지고, 마우스 오버 시 전체 이름·상세 툴팁이 노출되며 클릭 동작을 유지한다.
#[gpui::test]
fn status_bar_right_items_render_compact_icons_and_show_hover_tooltips(cx: &mut TestAppContext) {
    use krill_app::updater::{CheckStatus, StagedUpdate, UpdateState};
    use krill_proto::doctor::{Check, Fix, FixKind, Level, Status as DoctorStatus};
    use std::path::PathBuf;

    let (shell, handle) = open(cx);
    cx.update(|cx| {
        krill_app::onboarding::store_snapshot(
            cx,
            vec![Check {
                id: "git.identity".to_owned(),
                level: Level::Required,
                status: DoctorStatus::Fail,
                summary: "user.name 미설정".to_owned(),
                detail: String::new(),
                fix: Fix {
                    kind: FixKind::None,
                    value: String::new(),
                },
            }],
        );

        let staged = StagedUpdate {
            from_version: "0.1.0".to_owned(),
            to_version: "0.2.0".to_owned(),
            manifest_url: "http://127.0.0.1/latest.json".to_owned(),
            state_dir: PathBuf::from("/tmp/krill-state"),
            app_bundle: PathBuf::from("/tmp/Krill.app"),
            relaunched: false,
        };
        cx.set_global(UpdateState {
            status: CheckStatus::Ready(staged.clone()),
            staged: Some(staged),
            card_collapsed: true,
        });
    });
    shell.update(cx, |s, cx| {
        s.recording = krill_app::recording::Status::Unavailable;
        s.link = Link::Up;
        if let Some(pane) = s.layout.focused() {
            s.layout.set_session(pane, SessionId(1));
        }
        cx.notify();
    });
    cx.run_until_parked();

    let vcx = &mut VisualTestContext::from_window(handle, cx);

    // 아이콘 전용·점 전용 항목은 텍스트 라벨 없이 좁은 폭(≤ 32px)으로 축약되어야 한다.
    for icon_only_id in [
        "status-recording-warning",
        "status-daemon",
        "float-terminal",
        "dashboard-popout",
        "agent-history-toggle",
        "resources-toggle",
    ] {
        let b = vcx
            .debug_bounds(icon_only_id)
            .unwrap_or_else(|| panic!("missing {icon_only_id}"));
        assert!(
            b.size.width <= gpui::px(32.),
            "{icon_only_id} should be compact icon/dot, got width {:?}",
            b.size.width
        );
    }
    assert!(vcx.debug_bounds("status-sessions").is_some());
    assert!(vcx.debug_bounds("status-needs-you").is_some());

    let ssh_count = vcx.read(|cx| shell.read(cx).ssh_hosts.len().to_string());
    let ssh_full = text::SSH_MENU_BTN.format(&[("count", &ssh_count)]);
    let orch_full = text::ORCH_MENU_BTN.format(&[("runs", "0"), ("tasks", "0")]);
    let ports_full = text::PORTS_MENU_BTN.format(&[("count", "0")]);
    let checks_warn = krill_app::i18n::catalog::onboarding::STATUS_CHECKS_WARN.get();
    let update_badge = krill_app::i18n::catalog::updater_relaunch::STATUS_READY_BADGE
        .format(&[("version", "0.2.0")]);

    let expected_tooltips = [
        (
            "status-recording-warning",
            krill_app::i18n::catalog::recording::WARNING
                .get()
                .to_owned(),
        ),
        (
            "status-checks-warning",
            format!("{checks_warn}: user.name 미설정"),
        ),
        ("status-update-ready", update_badge),
        ("status-daemon", text::DAEMON_UP.get().to_owned()),
        ("status-usage", counts(0, 0)),
        (
            "status-orchestration-menu",
            format!("{} — {orch_full}", text::ORCH_MENU_TITLE.get()),
        ),
        (
            "status-ports-menu",
            format!("{} — {ports_full}", text::PORTS_MENU_TITLE.get()),
        ),
        (
            "status-ssh-switcher",
            format!("{} — {ssh_full}", text::SSH_MENU_TITLE.get()),
        ),
        (
            "float-terminal",
            format!("{} — {}", text::FLOAT_BUTTON.get(), text::FLOAT_LABEL.get()),
        ),
        (
            "dashboard-popout",
            format!(
                "{} — {}",
                text::DASHBOARD.get(),
                text::DASHBOARD_LABEL.get()
            ),
        ),
        (
            "agent-history-toggle",
            krill_app::i18n::catalog::agent_history::TITLE
                .get()
                .to_owned(),
        ),
        (
            "resources-toggle",
            krill_app::i18n::catalog::resources::TITLE.get().to_owned(),
        ),
    ];

    for (selector, expected_text) in expected_tooltips {
        let bounds = vcx
            .debug_bounds(selector)
            .unwrap_or_else(|| panic!("missing selector {selector}"));
        vcx.simulate_mouse_move(bounds.center(), None, gpui::Modifiers::default());
        vcx.executor().advance_clock(Duration::from_millis(600));
        vcx.run_until_parked();
        let tip_sel: &'static str = Box::leak(format!("tooltip:{selector}").into_boxed_str());
        let text_sel: &'static str =
            Box::leak(format!("tooltip-text:{selector}:{expected_text}").into_boxed_str());
        assert!(
            vcx.debug_bounds(tip_sel).is_some(),
            "hovering {selector} must show its tooltip"
        );
        assert!(
            vcx.debug_bounds(text_sel).is_some(),
            "tooltip for {selector} must render '{expected_text}'"
        );
    }

    // 업데이트 배지 클릭 시 업데이트 카드가 펼쳐지고, 리소스 아이콘 클릭 시 리소스 팝오버가 열린다.
    let update_bounds = vcx.debug_bounds("status-update-ready").unwrap();
    vcx.simulate_click(update_bounds.center(), gpui::Modifiers::default());
    vcx.run_until_parked();
    assert!(
        vcx.debug_bounds("update-card").is_some(),
        "clicking status-update-ready expands update card"
    );

    let resources_bounds = vcx.debug_bounds("resources-toggle").unwrap();
    vcx.simulate_click(resources_bounds.center(), gpui::Modifiers::default());
    vcx.run_until_parked();
    assert!(
        vcx.debug_bounds("resources-popover").is_some(),
        "clicking resources-toggle opens resources popover"
    );
}

/// OWE-851: 상태바 메뉴가 열려 있는 동안은 다른 버튼의 호버 툴팁을 숨겨 팝오버 글자를 가리지 않는다.
#[gpui::test]
fn status_menu_open_hides_sibling_tooltips(cx: &mut TestAppContext) {
    let (shell, handle) = open(cx);
    cx.run_until_parked();
    let vcx = &mut VisualTestContext::from_window(handle, cx);
    let ssh = vcx
        .debug_bounds("status-ssh-switcher")
        .expect("ssh menu button");
    vcx.simulate_click(ssh.center(), gpui::Modifiers::default());
    vcx.run_until_parked();
    assert_eq!(vcx.read(|cx| shell.read(cx).status_menus_open), 1);
    let refresh = vcx
        .debug_bounds("status-usage-refresh")
        .expect("refresh button");
    vcx.simulate_mouse_move(refresh.center(), None, gpui::Modifiers::default());
    vcx.executor().advance_clock(Duration::from_millis(600));
    vcx.run_until_parked();
    assert!(
        vcx.debug_bounds("tooltip:status-usage-refresh").is_none(),
        "open menu must hide sibling tooltips"
    );
    // 메뉴를 닫으면(Esc) 툴팁이 다시 나온다.
    vcx.simulate_keystrokes("escape");
    vcx.run_until_parked();
    assert_eq!(vcx.read(|cx| shell.read(cx).status_menus_open), 0);
    vcx.simulate_mouse_move(refresh.center(), None, gpui::Modifiers::default());
    vcx.executor().advance_clock(Duration::from_millis(600));
    vcx.run_until_parked();
    assert!(
        vcx.debug_bounds("tooltip:status-usage-refresh").is_some(),
        "closed menu must restore tooltips"
    );
}

/// OWE-857: SSH·오케스트레이션 메뉴 하단 버튼은 각 설정 목적에 맞는 라벨을 쓴다.
#[test]
fn settings_shortcut_labels_match_their_admin_tabs() {
    assert_eq!(text::OPEN_SSH_SETTINGS.get(), "SSH 호스트 설정…");
    assert_eq!(text::OPEN_WORKFLOWS_SETTINGS.get(), "워크플로 설정…");
    assert_ne!(
        text::OPEN_SSH_SETTINGS.get(),
        text::OPEN_ADMIN_ACCOUNTS.get()
    );
    assert_ne!(
        text::OPEN_WORKFLOWS_SETTINGS.get(),
        text::OPEN_ADMIN_ACCOUNTS.get()
    );
}
