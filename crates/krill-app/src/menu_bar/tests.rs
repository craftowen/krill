use std::collections::BTreeMap;
use std::io;
use std::sync::{Arc, Mutex};

use gpui::{AppContext as _, SystemNotificationResponse, TestAppContext, WindowOptions};
use krill_proto::control::AgentStatus;

use super::*;
use crate::sidebar::{Session, Sidebar};

#[derive(Clone)]
struct MemorySource(Arc<Mutex<Option<Snapshot>>>);

impl Source for MemorySource {
    fn fetch(&self) -> io::Result<Snapshot> {
        self.0
            .lock()
            .unwrap()
            .clone()
            .ok_or(io::ErrorKind::NotConnected.into())
    }
}

fn snapshot(states: &[(u64, AgentState)]) -> Snapshot {
    Snapshot {
        sessions: states
            .iter()
            .map(|&(id, state)| Session {
                id: SessionId(id),
                status: AgentStatus {
                    state,
                    agent: "claude".into(),
                    since_unix_ns: id,
                    ..AgentStatus::UNKNOWN
                },
                worktree: None,
                cwd: None,
                account: None,
            })
            .collect(),
        trees: BTreeMap::new(),
    }
}

fn tick(cx: &mut TestAppContext) {
    cx.executor().advance_clock(Duration::from_millis(500));
    cx.run_until_parked();
}

#[gpui::test]
fn closing_last_window_keeps_counts_alerts_and_click_route(cx: &mut TestAppContext) {
    let source = MemorySource(Arc::new(Mutex::new(Some(snapshot(&[(
        1,
        AgentState::Working,
    )])))));
    let clicked = Arc::new(Mutex::new(Vec::new()));
    let window = cx.update(|cx| {
        gpui_component::init(cx);
        crate::settings::init_at(None, cx);
        let clicked = clicked.clone();
        init(
            Arc::new(source.clone()),
            move |session, cx| {
                clicked.lock().unwrap().push(session);
                cx.open_window(WindowOptions::default(), |_, cx| cx.new(|_| gpui::Empty))
                    .unwrap();
            },
            cx,
        );
        cx.open_window(WindowOptions::default(), |_, cx| cx.new(|_| gpui::Empty))
            .unwrap()
    });
    cx.run_until_parked();
    assert_eq!(
        cx.read(counts),
        Some(Counts {
            working: 1,
            waiting: 0
        })
    );
    cx.update(|cx| {
        window
            .update(cx, |_, window, _| window.remove_window())
            .unwrap();
    });
    cx.run_until_parked();
    cx.update(|cx| assert!(cx.windows().is_empty()));

    *source.0.lock().unwrap() = Some(snapshot(&[
        (1, AgentState::Blocked),
        (2, AgentState::Waiting),
        (3, AgentState::Working),
        (4, AgentState::Done),
        (5, AgentState::Exited),
        (6, AgentState::Unknown),
    ]));
    tick(cx);
    assert_eq!(
        cx.read(counts),
        Some(Counts {
            working: 1,
            waiting: 2
        })
    );
    let shown = cx.shown_system_notifications().len();
    assert_eq!(shown, 2);
    tick(cx);
    assert_eq!(
        cx.shown_system_notifications().len(),
        shown,
        "안정된 상태는 중복 발송하지 않는다"
    );

    // 새 사이드바가 생겨도 전역 클릭 핸들러를 덮거나 과거 상태로 집계를 되돌리지 않는다.
    let sidebar = cx.new(|cx| {
        let mut sidebar = Sidebar::new(None);
        sidebar.connect_source(
            Arc::new(MemorySource(Arc::new(Mutex::new(
                Some(Snapshot::default()),
            )))),
            cx,
        );
        sidebar
    });
    cx.run_until_parked();
    assert_eq!(cx.read(counts).unwrap().waiting, 2);
    cx.simulate_system_notification_response(SystemNotificationResponse {
        tag: "pane:1".into(),
        action_id: None,
    });
    cx.run_until_parked();
    assert_eq!(*clicked.lock().unwrap(), [Some(SessionId(1))]);
    cx.update(|cx| assert_eq!(cx.windows().len(), 1));
    assert!(
        !cx.delivered_system_notifications()
            .iter()
            .any(|n| n.tag.as_ref() == "pane:1")
    );
    drop(sidebar);

    *source.0.lock().unwrap() = None;
    tick(cx);
    assert_eq!(
        cx.read(counts),
        None,
        "연결이 끊기면 오래된 수치를 표시하지 않는다"
    );
    *source.0.lock().unwrap() = Some(Snapshot::default());
    tick(cx);
    assert_eq!(cx.read(counts), Some(Counts::default()));
}

#[gpui::test]
fn monitor_is_singleton_and_respects_disabled_notifications(cx: &mut TestAppContext) {
    let source = MemorySource(Arc::new(Mutex::new(Some(snapshot(&[(
        1,
        AgentState::Blocked,
    )])))));
    let dir = std::env::temp_dir().join(format!("mk-menu-settings-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("settings.toml"),
        "notifications = false\nkeep_awake = \"off\"\n",
    )
    .unwrap();
    cx.update(|cx| {
        gpui_component::init(cx);
        crate::settings::init_at(Some(dir.clone()), cx);
        init(Arc::new(source), |_, _| {}, cx);
        init(
            Arc::new(MemorySource(Arc::new(Mutex::new(
                Some(Snapshot::default()),
            )))),
            |_, _| {},
            cx,
        );
    });
    cx.run_until_parked();
    assert_eq!(
        cx.read(counts).unwrap().waiting,
        1,
        "두 번째 모니터가 처음 것을 교체하지 않는다"
    );
    assert!(cx.shown_system_notifications().is_empty());
    std::fs::remove_dir_all(dir).unwrap();
}
