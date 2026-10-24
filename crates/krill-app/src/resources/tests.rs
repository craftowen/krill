use std::sync::Mutex;
use std::time::Duration;

use gpui::{TestAppContext, VisualTestContext};
use krill_proto::resources::SessionUsage;

use super::*;

fn sample() -> Resources {
    Resources {
        client: Some(40 << 20),
        daemon: 6 << 20,
        holder: Some(1 << 20),
        disk: 3 << 20,
        sessions: vec![SessionUsage {
            session: SessionId(2),
            footprint: 5 << 20,
            processes: 2,
        }],
    }
}

/// 받은 요청을 적고 조회에는 [`sample`]로 답하는 데몬 대역.
fn fake() -> (Call, Arc<Mutex<Vec<Request>>>) {
    let log = Arc::new(Mutex::new(Vec::new()));
    let seen = Arc::clone(&log);
    let call: Call = Arc::new(move |req| {
        seen.lock().unwrap().push(req.clone());
        Ok(match req {
            Request::Close { .. } => Reply::Closed,
            _ => Reply::Resources(sample()),
        })
    });
    (call, log)
}

fn open(cx: &mut TestAppContext, call: Call) -> (gpui::Entity<Panel>, &mut VisualTestContext) {
    cx.update(gpui_component::init);
    let (panel, cx) = cx.add_window_view(|_, _| Panel::new(call));
    (panel, cx)
}

#[test]
fn mb_rounds_to_tenths_like_footprint() {
    assert_eq!(mb(0), "0.0 MB");
    assert_eq!(mb(5_603_880), "5.3 MB");
    assert_eq!(mb(1 << 20), "1.0 MB");
    assert_eq!(mb(u64::MAX), mb(u64::MAX)); // 넘침 없이 끝난다
}

#[gpui::test]
fn expanded_panel_polls_and_collapsed_does_not(cx: &mut TestAppContext) {
    let (call, log) = fake();
    let (panel, cx) = open(cx, call);
    cx.run_until_parked();
    assert!(log.lock().unwrap().is_empty(), "접힌 동안은 묻지 않는다");
    panel.update(cx, Panel::toggle);
    cx.run_until_parked();
    assert_eq!(*log.lock().unwrap(), [Request::Resources(Action::Query)]);
    panel.read_with(cx, |p, _| assert_eq!(p.data, Some(sample())));
}

#[gpui::test]
fn restart_and_kill_only_after_confirmation(cx: &mut TestAppContext) {
    let (call, log) = fake();
    let (panel, cx) = open(cx, call);

    panel.update_in(cx, Panel::confirm_restart);
    let (title, _) = cx.pending_prompt().expect("restart dialog");
    assert_eq!(title, text::RESTART_TITLE.get());
    cx.simulate_prompt_answer(text::CANCEL.get());
    cx.run_until_parked();
    assert!(log.lock().unwrap().is_empty(), "취소하면 보내지 않는다");

    panel.update_in(cx, Panel::confirm_restart);
    cx.simulate_prompt_answer(text::RESTART.get());
    cx.run_until_parked();
    assert_eq!(
        *log.lock().unwrap(),
        [Request::Resources(Action::RestartDaemon)]
    );
    panel.read_with(cx, |p, _| {
        assert_eq!(p.note.as_deref(), Some(text::RESTART_SENT.get()));
    });

    panel.update_in(cx, |p, window, cx| p.confirm_kill(SessionId(2), window, cx));
    let (title, _) = cx.pending_prompt().expect("kill dialog");
    let fallback = format!("{} 2", text::SESSION.get());
    assert_eq!(
        title,
        text::KILL_TITLE_NAMED.format(&[("name", &fallback)]),
        "이름을 모르면 세션 번호로 묻는다"
    );
    cx.simulate_prompt_answer(text::CANCEL.get());
    cx.run_until_parked();
    assert_eq!(log.lock().unwrap().len(), 1);
    panel.update_in(cx, |p, window, cx| p.confirm_kill(SessionId(2), window, cx));
    cx.simulate_prompt_answer(text::KILL_CONFIRM.get());
    cx.run_until_parked();
    assert_eq!(
        log.lock().unwrap().last(),
        Some(&Request::Close {
            session: SessionId(2)
        })
    );
}

#[test]
fn old_daemon_without_feature_gets_no_request() {
    let mut wire = std::io::Cursor::new(Vec::new());
    let req = Request::Resources(Action::Query);
    let err = exchange(FEAT_CONTROL, &mut wire, &req).unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::Unsupported);
    assert!(
        wire.get_ref().is_empty(),
        "미지원 데몬에는 한 바이트도 쓰지 않는다"
    );
}

#[gpui::test]
fn unsupported_daemon_shows_update_needed(cx: &mut TestAppContext) {
    let call: Call = Arc::new(|_| Err(io::ErrorKind::Unsupported.into()));
    let (panel, cx) = open(cx, call);
    panel.update(cx, Panel::toggle);
    cx.run_until_parked();
    panel.read_with(cx, |p, _| assert!(p.outdated && p.data.is_none()));
}

/// OWE-851: 행은 `세션 N` 대신 에이전트·워크트리 이름을 보이고, 클릭하면 Open을 낸 뒤 닫힌다.
#[gpui::test]
fn session_row_shows_name_and_click_emits_open(cx: &mut TestAppContext) {
    use std::path::PathBuf;

    use krill_proto::control::{AgentState, AgentStatus};
    struct Memory(crate::sidebar::Snapshot);
    impl crate::sidebar::Source for Memory {
        fn fetch(&self) -> std::io::Result<crate::sidebar::Snapshot> {
            Ok(self.0.clone())
        }
    }
    let sidebar = cx.update(|cx| cx.new(|_| crate::sidebar::Sidebar::new(None)));
    sidebar.update(cx, |s, cx| {
        s.connect_source(
            Arc::new(Memory(crate::sidebar::Snapshot {
                sessions: vec![crate::sidebar::Session {
                    id: SessionId(2),
                    status: AgentStatus {
                        agent: "codex".into(),
                        state: AgentState::Working,
                        ..AgentStatus::UNKNOWN
                    },
                    worktree: Some(PathBuf::from("/repo/alpha")),
                    cwd: None,
                    account: None,
                }],
                ..Default::default()
            })),
            cx,
        );
    });
    let (call, _) = fake();
    let (panel, cx) = open(cx, call);
    panel.update(cx, |p, _| p.set_sidebar(&sidebar));
    let opened: Arc<Mutex<Vec<SessionId>>> = Arc::new(Mutex::new(Vec::new()));
    let opened_clone = Arc::clone(&opened);
    cx.update(|_, cx| {
        cx.subscribe(&panel, move |_, event: &Open, _| {
            opened_clone.lock().unwrap().push(event.0);
        })
        .detach();
    });
    panel.update(cx, Panel::toggle);
    cx.run_until_parked();
    let row = cx
        .debug_bounds("resources-session-2")
        .expect("named session row");
    cx.simulate_click(row.center(), gpui::Modifiers::default());
    cx.run_until_parked();
    assert_eq!(*opened.lock().unwrap(), [SessionId(2)]);
    panel.read_with(cx, |p, _| assert!(!p.open));

    // OWE-851b: 종료 확인 제목도 번호 대신 세션 이름을 쓴다.
    panel.update_in(cx, |p, window, cx| p.confirm_kill(SessionId(2), window, cx));
    let (title, _) = cx.pending_prompt().expect("kill dialog");
    assert_eq!(title, text::KILL_TITLE_NAMED.format(&[("name", "codex")]));
    cx.simulate_prompt_answer(text::CANCEL.get());
}

/// OWE-652: 바깥 클릭은 팝오버를 닫고 폴링을 멈춘다.
#[gpui::test]
fn outside_click_closes_popover_and_stops_poll(cx: &mut TestAppContext) {
    let (call, log) = fake();
    let (panel, cx) = open(cx, call);
    panel.update(cx, Panel::toggle);
    cx.run_until_parked();
    panel.read_with(cx, |p, _| assert!(p.open));
    assert!(
        cx.debug_bounds("resources-popover").is_some(),
        "popover paints above the status bar"
    );
    assert_eq!(log.lock().unwrap().len(), 1);
    let toggle = cx.debug_bounds("resources-toggle").expect("toggle");
    // 열린 상태에서 헤더를 누르면 바깥 닫기와 같은 제스처라 닫힌 채로 둔다.
    cx.simulate_click(toggle.center(), gpui::Modifiers::default());
    cx.run_until_parked();
    panel.read_with(cx, |p, _| assert!(!p.open));
    // 닫힌 상태에서 헤더를 누르면 다시 열린다.
    cx.simulate_click(toggle.center(), gpui::Modifiers::default());
    cx.run_until_parked();
    panel.read_with(cx, |p, _| assert!(p.open));
    let size = cx.update(|window, _| window.viewport_size());
    // 헤더(왼쪽 위 한 줄)와 팝오버(오른쪽 260px)를 모두 피한 왼쪽 아래를 누른다.
    assert!(
        size.width > gpui::px(300.) && size.height > gpui::px(200.),
        "window must fit popover + away point, got {size:?}"
    );
    let away = gpui::point(gpui::px(10.), size.height - gpui::px(10.));
    cx.simulate_click(away, gpui::Modifiers::default());
    cx.run_until_parked();
    panel.read_with(cx, |p, _| assert!(!p.open));
    cx.executor().advance_clock(Duration::from_secs(10));
    cx.run_until_parked();
    assert_eq!(log.lock().unwrap().len(), 2, "closed panel does not poll");
}

/// OWE-856: 상태바 토글은 버튼 역할·이름을 노출한다.
#[gpui::test]
fn toggle_exposes_button_role_and_label(cx: &mut TestAppContext) {
    let (call, _) = fake();
    let (panel, cx) = open(cx, call);
    let (role, label) = cx.update(|_, cx| {
        panel.update(cx, |p, cx| {
            let el = p.render_toggle(cx);
            let mut node = gpui::accesskit::Node::new(gpui::Role::Unknown);
            gpui::Element::write_a11y_info(&el, &mut node);
            (
                gpui::Element::a11y_role(&el),
                node.label().map(str::to_owned),
            )
        })
    });
    assert_eq!(role, Some(gpui::Role::Button));
    assert_eq!(label.as_deref(), Some(text::TITLE.get()));
}

#[gpui::test]
fn hidden_resource_panel_pauses_measurement_and_catches_up_on_return(cx: &mut TestAppContext) {
    let (call, log) = fake();
    let (panel, cx) = open(cx, call);
    panel.update(cx, Panel::toggle);
    cx.run_until_parked();
    assert_eq!(log.lock().unwrap().len(), 1);
    panel.update(cx, |panel, cx| panel.set_visible(false, cx));
    cx.executor().advance_clock(Duration::from_secs(10));
    cx.run_until_parked();
    assert_eq!(log.lock().unwrap().len(), 1);
    panel.read_with(cx, |panel, _| assert!(panel.is_open()));
    panel.update(cx, |panel, cx| panel.set_visible(true, cx));
    cx.run_until_parked();
    assert_eq!(log.lock().unwrap().len(), 2);
}

#[gpui::test]
fn ui_audit_resources_many_sessions_stay_in_viewport(cx: &mut TestAppContext) {
    let (call, _) = fake();
    let (panel, vcx) = open(cx, call);
    vcx.simulate_resize(gpui::size(gpui::px(640.), gpui::px(420.)));
    panel.update(vcx, |p, cx| {
        let mut data = sample();
        data.sessions = (1..=100)
            .map(|id| SessionUsage {
                session: SessionId(id),
                footprint: 1024,
                processes: 1,
            })
            .collect();
        p.data = Some(data);
        p.open = true;
        cx.notify();
    });
    vcx.run_until_parked();
    let popover = vcx.debug_bounds("resources-popover").expect("popover");
    assert!(popover.size.height <= gpui::px(420.) - crate::ui::SPACE_4 * 3.);
}
