use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::PathBuf;
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use gpui::{AppContext as _, SystemNotificationResponse, TestAppContext, WindowOptions};
use krill_proto::control::{AgentState, AgentStatus};

use super::*;
use crate::sidebar::Source;

static SERIAL: Mutex<()> = Mutex::new(());

#[gpui::test]
fn initial_config_read_cannot_enable_notifications_or_power_from_defaults(cx: &mut TestAppContext) {
    let dir = temp_dir("config-loading");
    fs::write(
        dir.join("settings.toml"),
        "notifications = false\nkeep_awake = \"off\"\n",
    )
    .unwrap();
    cx.update(|cx| {
        gpui_component::init(cx);
        settings::init_at(Some(dir.clone()), cx);
        assert!(!settings::is_loaded(cx));
        sync_snapshot(
            &snapshot(vec![session(1, AgentState::Working, 1, "/repo")]),
            cx,
        );
        assert!(!is_preventing_sleep(cx));
        sync_snapshot(
            &snapshot(vec![session(1, AgentState::Done, 2, "/repo")]),
            cx,
        );
    });
    assert!(cx.shown_system_notifications().is_empty());
    cx.run_until_parked();
    assert!(cx.shown_system_notifications().is_empty());
    cx.read(|cx| assert!(!is_preventing_sleep(cx)));
    fs::remove_dir_all(dir).ok();
}

#[gpui::test]
fn daemon_owned_webhooks_are_not_duplicated_by_app_snapshots(cx: &mut TestAppContext) {
    let _guard = SERIAL
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let dir = temp_dir("webhook");
    let (url, handle) = crate::webhook::tests::server(vec![]);
    let settings = format!(
        "notifications = false\nkeep_awake = \"off\"\n[[webhooks]]\nprovider = \"ntfy\"\nurl = \"{url}\"\n"
    );
    fs::write(dir.join("settings.toml"), &settings).unwrap();
    init_settings(cx, &dir);
    let sync = |state, since, cx: &mut App| {
        let mut s = session(51, state, since, "/private/TERMINAL_SECRET");
        s.status.agent = "AGENT_SECRET".into();
        sync_snapshot(&snapshot(vec![s]), cx);
    };
    cx.update(|cx| {
        // 초기 Done과 완료가 아닌 상태는 보내지 않는다.
        sync(AgentState::Done, 1, cx);
        sync(AgentState::Waiting, 2, cx);
        sync(AgentState::Blocked, 3, cx);
        sync(AgentState::Exited, 4, cx);
        sync(AgentState::Unknown, 5, cx);
        sync(AgentState::Working, 6, cx);
        sync(AgentState::Done, 7, cx);
        sync(AgentState::Done, 7, cx);
        sync(AgentState::Done, 8, cx);
        // 다음 작업이 입력 대기를 거쳐 완료돼도 보낸다.
        sync(AgentState::Working, 9, cx);
        sync(AgentState::Waiting, 10, cx);
        sync(AgentState::Done, 11, cx);
    });
    cx.run_until_parked();
    let (listener, requests) = handle.join().unwrap();
    assert!(requests.is_empty());
    assert!(cx.shown_system_notifications().is_empty());

    // opt-in 제거는 즉시 반영하고 다시 켜도 과거 완료를 재전송하지 않는다.
    fs::write(dir.join("settings.toml"), "keep_awake = \"off\"\n").unwrap();
    cx.update(settings::reload);
    cx.run_until_parked();
    cx.update(|cx| {
        assert!(settings::current(cx).webhooks.is_empty());
        sync(AgentState::Working, 12, cx);
        sync(AgentState::Done, 13, cx);
    });
    fs::write(dir.join("settings.toml"), settings).unwrap();
    cx.update(settings::reload);
    cx.run_until_parked();
    cx.update(|cx| {
        sync(AgentState::Done, 13, cx);
    });
    cx.run_until_parked();
    assert_eq!(
        listener.accept().unwrap_err().kind(),
        io::ErrorKind::WouldBlock
    );
    fs::remove_dir_all(dir).ok();
}

#[derive(Clone)]
struct SharedSource(Arc<Mutex<Snapshot>>);

impl SharedSource {
    fn new(initial: Snapshot) -> Self {
        Self(Arc::new(Mutex::new(initial)))
    }

    fn set(&self, snapshot: Snapshot) {
        if let Ok(mut guard) = self.0.lock() {
            *guard = snapshot;
        }
    }
}

impl Source for SharedSource {
    fn fetch(&self) -> io::Result<Snapshot> {
        Ok(self
            .0
            .lock()
            .map_or_else(|_| Snapshot::default(), |g| g.clone()))
    }
}

fn session(id: u64, state: AgentState, since_unix_ns: u64, tree: &str) -> Session {
    Session {
        id: SessionId(id),
        status: AgentStatus {
            state,
            agent: "claude".into(),
            since_unix_ns,
            ..AgentStatus::UNKNOWN
        },
        worktree: Some(PathBuf::from(tree)),
        cwd: Some(PathBuf::from(tree)),
        account: None,
    }
}

fn snapshot(sessions: Vec<Session>) -> Snapshot {
    Snapshot {
        sessions,
        trees: BTreeMap::new(),
    }
}

fn init_settings(cx: &mut TestAppContext, dir: &Path) {
    cx.update(gpui_component::init);
    cx.update(|cx| settings::init_at(Some(dir.to_path_buf()), cx));
    cx.run_until_parked();
}

fn temp_dir(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("mk-alert-{}-{name}", std::process::id()));
    fs::remove_dir_all(&d).ok();
    fs::create_dir_all(&d).unwrap();
    d
}

fn pmset_assertions_output(cx: &TestAppContext) -> String {
    // 이 실제 OS 통합 검사에서만 적용 완료 배리어를 기다린다.
    let (done, applied) = mpsc::channel();
    let queued = cx.read(|cx| {
        cx.global::<AlertState>()
            .awake
            .as_ref()
            .is_some_and(|worker| worker.send.send(AwakeCommand::Barrier(done)).is_ok())
    });
    if queued {
        applied.recv_timeout(Duration::from_secs(5)).unwrap();
    }
    let out = Command::new("/usr/bin/pmset")
        .args(["-g", "assertions"])
        .output()
        .expect("pmset -g assertions should execute on macOS");
    assert!(out.status.success());
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn has_our_sleep_assertion(pmset_out: &str) -> bool {
    let pid_token = format!("pid {}", std::process::id());
    pmset_out.lines().any(|line| {
        line.contains(&pid_token)
            && line.contains("PreventUserIdleSystemSleep")
            && line.contains(SLEEP_REASON)
    })
}

#[test]
fn pane_tag_roundtrip_and_keep_awake_matrix() {
    let tag = pane_tag(SessionId(42));
    assert_eq!(tag.as_ref(), "pane:42");
    assert_eq!(parse_pane_tag(tag.as_ref()), Some(SessionId(42)));
    assert_eq!(parse_pane_tag("pane:"), None);
    assert_eq!(parse_pane_tag("other:42"), None);

    assert!(should_keep_awake(KeepAwake::On, 0));
    assert!(should_keep_awake(KeepAwake::On, 3));
    assert!(!should_keep_awake(KeepAwake::Agent, 0));
    assert!(should_keep_awake(KeepAwake::Agent, 1));
    assert!(!should_keep_awake(KeepAwake::Off, 0));
    assert!(!should_keep_awake(KeepAwake::Off, 5));
}

#[gpui::test]
fn waiting_blocked_done_notifications_badge_and_click(cx: &mut TestAppContext) {
    let _guard = SERIAL
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let dir = temp_dir("flow");
    init_settings(cx, &dir);

    // 초기 스냅샷: 세션 1은 Working, 세션 2는 이미 Done(시작 시점의 기존 Done은 알림을 띄우지 않는다).
    let source = SharedSource::new(snapshot(vec![
        session(1, AgentState::Working, 10, "/repo/wt-alpha"),
        session(2, AgentState::Done, 10, "/repo/wt-beta"),
    ]));
    let selected: Arc<Mutex<Vec<SessionId>>> = Arc::new(Mutex::new(Vec::new()));
    let sidebar = cx.new(|cx| {
        let mut s = Sidebar::new(None);
        s.connect_source(Arc::new(source.clone()), cx);
        s
    });
    cx.update(|cx| {
        let selected = selected.clone();
        cx.subscribe(&sidebar, move |_, e: &Selected, _| {
            selected.lock().unwrap().push(e.0);
        })
        .detach();
    });
    cx.run_until_parked();
    assert!(cx.delivered_system_notifications().is_empty());
    assert_eq!(cx.read(dock_badge), None);

    // 500ms(< 5초) 폴링 틱 안에 세션 1이 Waiting, 세션 2가 Blocked로 전환되면 알림과 배지(2)가 반영된다.
    source.set(snapshot(vec![
        session(1, AgentState::Waiting, 20, "/repo/wt-alpha"),
        session(2, AgentState::Blocked, 25, "/repo/wt-beta"),
    ]));
    cx.executor().advance_clock(Duration::from_millis(500));
    cx.run_until_parked();

    let delivered = cx.delivered_system_notifications();
    assert_eq!(delivered.len(), 2);
    assert!(
        delivered
            .iter()
            .any(|n| n.tag.as_ref() == "pane:1" && n.title.contains("wt-alpha"))
    );
    assert!(
        delivered
            .iter()
            .any(|n| n.tag.as_ref() == "pane:2" && n.title.contains("wt-beta"))
    );
    assert_eq!(cx.read(dock_badge).as_deref(), Some("2"));
    assert_eq!(platform::dock_badge().as_deref(), Some("2"));

    // 같은 상태·시각이 반복 폴링돼도 중복 알림을 보내지 않는다.
    let shown_before = cx.shown_system_notifications().len();
    cx.executor().advance_clock(Duration::from_millis(500));
    cx.run_until_parked();
    assert_eq!(cx.shown_system_notifications().len(), shown_before);

    // 세션 1이 다시 Working으로 돌아가면 "pane:1" 알림이 자동으로 걷히고 배지가 1로 줄어든다.
    source.set(snapshot(vec![
        session(1, AgentState::Working, 30, "/repo/wt-alpha"),
        session(2, AgentState::Blocked, 25, "/repo/wt-beta"),
    ]));
    cx.executor().advance_clock(Duration::from_millis(500));
    cx.run_until_parked();
    let delivered = cx.delivered_system_notifications();
    assert_eq!(delivered.len(), 1);
    assert_eq!(delivered[0].tag.as_ref(), "pane:2");
    assert_eq!(cx.read(dock_badge).as_deref(), Some("1"));

    // 세션 1이 Working -> Done으로 끝나면 완료 알림("pane:1")이 나가고 배지는 Needs You(1)만 유지한다.
    source.set(snapshot(vec![
        session(1, AgentState::Done, 40, "/repo/wt-alpha"),
        session(2, AgentState::Blocked, 25, "/repo/wt-beta"),
    ]));
    cx.executor().advance_clock(Duration::from_millis(500));
    cx.run_until_parked();
    let delivered = cx.delivered_system_notifications();
    assert_eq!(delivered.len(), 2);
    let done_notif = delivered
        .iter()
        .find(|n| n.tag.as_ref() == "pane:1")
        .cloned()
        .unwrap();
    assert!(done_notif.title.contains(text::DONE.get()));
    assert_eq!(cx.read(dock_badge).as_deref(), Some("1"));

    // 알림 클릭 시 해당 세션(SessionId(1))으로 Selected 이벤트가 발행되고 알림이 닫힌다.
    cx.simulate_system_notification_response(SystemNotificationResponse {
        tag: done_notif.tag,
        action_id: None,
    });
    cx.run_until_parked();
    assert_eq!(*selected.lock().unwrap(), vec![SessionId(1)]);
    assert!(
        !cx.delivered_system_notifications()
            .iter()
            .any(|n| n.tag.as_ref() == "pane:1")
    );

    // 정리: 세션을 모두 비워 assertion과 배지를 초기화한다.
    source.set(Snapshot::default());
    cx.executor().advance_clock(Duration::from_millis(500));
    cx.run_until_parked();
    fs::remove_dir_all(&dir).ok();
}

#[gpui::test]
fn settings_toggle_and_pmset_iopm_assertion(cx: &mut TestAppContext) {
    let _guard = SERIAL
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let dir = temp_dir("pmset");
    fs::write(dir.join("settings.toml"), r#"keep_awake = "agent""#).unwrap();
    init_settings(cx, &dir);

    // 기본값: notifications = true, keep_awake = "agent". Working 세션이 있으면 pmset에 표시된다.
    cx.update(|cx| {
        sync_snapshot(
            &snapshot(vec![session(7, AgentState::Working, 1, "/repo/wt-work")]),
            cx,
        );
        assert!(is_preventing_sleep(cx));
    });
    let pmset_working = pmset_assertions_output(cx);
    assert!(
        has_our_sleep_assertion(&pmset_working),
        "expected PreventUserIdleSystemSleep for pid {} in pmset output:\n{pmset_working}",
        std::process::id()
    );

    // Working -> Waiting 전환: Agent 모드이므로 assertion이 즉시 해제되고 알림·배지가 켜진다.
    cx.update(|cx| {
        sync_snapshot(
            &snapshot(vec![session(7, AgentState::Waiting, 2, "/repo/wt-work")]),
            cx,
        );
        assert!(!is_preventing_sleep(cx));
        assert_eq!(dock_badge(cx).as_deref(), Some("1"));
    });
    assert_eq!(cx.delivered_system_notifications().len(), 1);
    let pmset_waiting = pmset_assertions_output(cx);
    assert!(
        !has_our_sleep_assertion(&pmset_waiting),
        "expected sleep assertion to be released when no session is Working:\n{pmset_waiting}"
    );

    // settings.toml에서 notifications = false, keep_awake = "on"으로 바꾸면 알림·배지가 꺼지고
    // Working 세션이 없어도 IOPMAssertion이 잡힌다.
    fs::write(
        dir.join("settings.toml"),
        "notifications = false\nkeep_awake = \"on\"\n",
    )
    .unwrap();
    cx.update(settings::reload);
    cx.run_until_parked();
    cx.update(|cx| {
        assert!(is_preventing_sleep(cx));
        assert_eq!(dock_badge(cx), None);
    });
    assert!(cx.delivered_system_notifications().is_empty());
    let pmset_on = pmset_assertions_output(cx);
    assert!(
        has_our_sleep_assertion(&pmset_on),
        "expected PreventUserIdleSystemSleep under keep_awake = \"on\":\n{pmset_on}"
    );

    // keep_awake = "off"면 Working 세션이 있어도 assertion을 잡지 않는다.
    fs::write(
        dir.join("settings.toml"),
        "notifications = false\nkeep_awake = \"off\"\n",
    )
    .unwrap();
    cx.update(settings::reload);
    cx.run_until_parked();
    cx.update(|cx| {
        sync_snapshot(
            &snapshot(vec![session(7, AgentState::Working, 3, "/repo/wt-work")]),
            cx,
        );
        assert!(!is_preventing_sleep(cx));
    });
    let pmset_off = pmset_assertions_output(cx);
    assert!(
        !has_our_sleep_assertion(&pmset_off),
        "expected no sleep assertion under keep_awake = \"off\":\n{pmset_off}"
    );

    // NSDockTile 브리지 직접 왕복 검증.
    assert_eq!(platform::sync_dock_tile(Some("4")).as_deref(), Some("4"));
    assert_eq!(platform::sync_dock_tile(None), None);

    fs::remove_dir_all(&dir).ok();
}

#[gpui::test]
fn startup_and_reconnect_are_baselines_and_active_focus_is_silent(cx: &mut TestAppContext) {
    let dir = temp_dir("baseline");
    // 포커스 억제는 창이 있을 때만 적용된다(메뉴바 상주). 억제 구간은 창을 열어 둔다.
    init_settings(cx, &dir);
    cx.update(|cx| settings::edit(cx, |s| s.keep_awake = settings::KeepAwake::Agent));
    let window = cx.update(|cx| {
        cx.open_window(WindowOptions::default(), |_, cx| cx.new(|_| gpui::Empty))
            .unwrap()
    });
    cx.update(|cx| {
        let first = snapshot(vec![session(1, AgentState::Waiting, 1, "/repo/a")]);
        sync_snapshot(&first, cx);
        assert_eq!(dock_badge(cx).as_deref(), Some("1"));
        focus(Some(SessionId(1)), cx);
        set_active(true, cx);
        sync_snapshot(
            &snapshot(vec![session(1, AgentState::Blocked, 2, "/repo/a")]),
            cx,
        );
    });
    assert!(cx.shown_system_notifications().is_empty());
    cx.update(|cx| {
        set_active(false, cx);
        // 활성 창에서 이미 소비한 전이는 창이 비활성화돼도 다시 보내지 않는다.
        sync_snapshot(
            &snapshot(vec![session(1, AgentState::Blocked, 2, "/repo/a")]),
            cx,
        );
    });
    assert!(cx.shown_system_notifications().is_empty());
    cx.update(|cx| {
        sync_snapshot(
            &snapshot(vec![session(1, AgentState::Waiting, 3, "/repo/a")]),
            cx,
        );
    });
    assert_eq!(cx.delivered_system_notifications().len(), 1);
    cx.update(|cx| {
        sync_snapshot(
            &snapshot(vec![session(1, AgentState::Working, 4, "/repo/a")]),
            cx,
        );
        assert!(is_preventing_sleep(cx));
        disconnected(cx);
        assert_eq!(cx.global::<AlertState>().working_count, 0);
        assert_eq!(dock_badge(cx), None);
        assert!(!is_preventing_sleep(cx));
        sync_snapshot(
            &snapshot(vec![session(1, AgentState::Waiting, 5, "/repo/a")]),
            cx,
        );
    });
    assert!(cx.delivered_system_notifications().is_empty());
    assert_eq!(cx.shown_system_notifications().len(), 1);
    cx.update(|cx| {
        window
            .update(cx, |_, window, _| window.remove_window())
            .unwrap();
    });
    cx.run_until_parked();
    fs::remove_dir_all(dir).unwrap();
}

#[gpui::test]
fn closed_window_does_not_suppress_focused_session(cx: &mut TestAppContext) {
    let dir = temp_dir("closed-window");
    init_settings(cx, &dir);
    let window = cx.update(|cx| {
        cx.open_window(WindowOptions::default(), |_, cx| cx.new(|_| gpui::Empty))
            .unwrap()
    });
    cx.update(|cx| {
        sync_snapshot(
            &snapshot(vec![session(1, AgentState::Working, 1, "/repo/a")]),
            cx,
        );
        focus(Some(SessionId(1)), cx);
        set_active(true, cx);
        sync_snapshot(
            &snapshot(vec![session(1, AgentState::Waiting, 2, "/repo/a")]),
            cx,
        );
    });
    assert!(
        cx.shown_system_notifications().is_empty(),
        "활성 창의 포커스 칸은 알림을 생략한다"
    );
    // 마지막 창을 닫아도 stale한 active·focused가 남아 있다. 창이 없으면 알림을 보내야 한다.
    cx.update(|cx| {
        window
            .update(cx, |_, window, _| window.remove_window())
            .unwrap();
    });
    cx.run_until_parked();
    cx.update(|cx| assert!(cx.windows().is_empty()));
    cx.update(|cx| {
        sync_snapshot(
            &snapshot(vec![session(1, AgentState::Blocked, 3, "/repo/a")]),
            cx,
        );
    });
    assert_eq!(
        cx.delivered_system_notifications().len(),
        1,
        "창이 없으면 포커스 칸도 알림을 보낸다"
    );
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn suppress_notification_truth_table() {
    // 활성 창이 없으면(메뉴바 상주 포함) 항상 보낸다.
    assert!(!suppress_notification(true, true, false));
    assert!(!suppress_notification(true, false, false));
    assert!(!suppress_notification(false, true, false));
    // 비활성 시 모드: 활성 창이 있으면 포커스 여부와 관계없이 생략한다.
    assert!(suppress_notification(true, false, true));
    assert!(suppress_notification(true, true, true));
    // 기본 모드: 보고 있는 칸만 생략한다.
    assert!(suppress_notification(false, true, true));
    assert!(!suppress_notification(false, false, true));
}

#[gpui::test]
fn inactive_only_mode_suppresses_while_a_window_is_active(cx: &mut TestAppContext) {
    let _guard = SERIAL
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let dir = temp_dir("inactive-only");
    fs::write(
        dir.join("settings.toml"),
        "notifications = true\nkeep_awake = \"off\"\nnotify_only_inactive = true\n",
    )
    .unwrap();
    init_settings(cx, &dir);
    cx.update(|cx| {
        cx.open_window(WindowOptions::default(), |_, cx| cx.new(|_| gpui::Empty))
            .unwrap();
    });
    cx.update(|cx| {
        sync_snapshot(
            &snapshot(vec![session(7, AgentState::Working, 1, "/repo/a")]),
            cx,
        );
        set_active(true, cx);
        sync_snapshot(
            &snapshot(vec![session(7, AgentState::Waiting, 2, "/repo/a")]),
            cx,
        );
    });
    assert!(
        cx.shown_system_notifications().is_empty(),
        "비활성 시 모드에서는 활성 창이 있으면 알림을 생략한다"
    );
    // 창이 비활성이면 같은 세션도 알림을 보낸다.
    cx.update(|cx| {
        set_active(false, cx);
        sync_snapshot(
            &snapshot(vec![session(7, AgentState::Waiting, 3, "/repo/a")]),
            cx,
        );
    });
    assert_eq!(cx.shown_system_notifications().len(), 1);
    // 모드를 끄면 활성 창에서도(포커스 칸이 아니면) 알림을 보낸다.
    fs::write(
        dir.join("settings.toml"),
        "notifications = true\nkeep_awake = \"off\"\nnotify_only_inactive = false\n",
    )
    .unwrap();
    cx.update(settings::reload);
    cx.run_until_parked();
    cx.update(|cx| {
        set_active(true, cx);
        sync_snapshot(
            &snapshot(vec![session(7, AgentState::Waiting, 4, "/repo/a")]),
            cx,
        );
    });
    assert_eq!(cx.shown_system_notifications().len(), 2);
    fs::remove_dir_all(dir).ok();
}

#[test]
fn notify_event_matrix_and_legacy_defaults() {
    use super::AlertEvent;
    const ALL: [AlertEvent; 3] = [AlertEvent::InputWait, AlertEvent::Done, AlertEvent::Bell];

    // 입력·완료는 기본 켬, 벨은 기본 끔. 행렬은 벨도 명시적으로 켠 상태로 검사한다.
    assert!(!settings::Settings::default().notify_on_bell);
    let all_on = settings::Settings {
        notify_on_bell: true,
        ..settings::Settings::default()
    };
    assert!(all_on.notify_on_input && all_on.notify_on_done && all_on.notify_on_bell);
    for event in ALL {
        assert!(should_notify(&all_on, event), "{event:?}");
    }

    // 마스터를 끄면 이벤트 토글과 관계없이 전부 끈다.
    let master_off = settings::Settings {
        notifications: false,
        ..settings::Settings::default()
    };
    for event in ALL {
        assert!(!should_notify(&master_off, event), "{event:?}");
    }

    // 각 토글을 하나씩 끄면 해당 이벤트만 꺼진다.
    let off_input = settings::Settings {
        notify_on_input: false,
        notify_on_bell: true,
        ..settings::Settings::default()
    };
    assert!(!should_notify(&off_input, AlertEvent::InputWait));
    assert!(should_notify(&off_input, AlertEvent::Done));
    assert!(should_notify(&off_input, AlertEvent::Bell));
    let off_done = settings::Settings {
        notify_on_done: false,
        notify_on_bell: true,
        ..settings::Settings::default()
    };
    assert!(should_notify(&off_done, AlertEvent::InputWait));
    assert!(!should_notify(&off_done, AlertEvent::Done));
    assert!(should_notify(&off_done, AlertEvent::Bell));
    let off_bell = settings::Settings {
        notify_on_bell: false,
        ..settings::Settings::default()
    };
    assert!(should_notify(&off_bell, AlertEvent::InputWait));
    assert!(should_notify(&off_bell, AlertEvent::Done));
    assert!(!should_notify(&off_bell, AlertEvent::Bell));

    // 기존 settings.toml(키 없음)은 켬으로 읽히고, 왕복해도 같다.
    let legacy = settings::Settings::parse("notifications = true\n").unwrap();
    assert!(legacy.notify_on_input && legacy.notify_on_done && !legacy.notify_on_bell);
    assert_eq!(
        settings::Settings::parse(&all_on.to_toml()).unwrap(),
        all_on
    );
}

/// OWE-659: 이벤트 토글이 꺼지면 스냅샷·벨 알림이 나가지 않고, 켜면 다시 나간다.
/// 배지는 마스터 토글만 따르고 이벤트 토글에는 묶이지 않는다.
#[gpui::test]
fn event_toggles_gate_snapshot_and_bell(cx: &mut TestAppContext) {
    let _guard = SERIAL
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let dir = temp_dir("owe-659");
    fs::write(
        dir.join("settings.toml"),
        "notifications = true\nnotify_on_input = false\nnotify_on_done = false\nnotify_on_bell = false\nkeep_awake = \"off\"\n",
    )
    .unwrap();
    init_settings(cx, &dir);
    cx.update(|cx| {
        sync_snapshot(
            &snapshot(vec![session(21, AgentState::Working, 1, "/repo/a")]),
            cx,
        );
        sync_snapshot(
            &snapshot(vec![session(21, AgentState::Waiting, 2, "/repo/a")]),
            cx,
        );
    });
    assert!(cx.shown_system_notifications().is_empty());
    assert_eq!(cx.read(dock_badge).as_deref(), Some("1"));
    cx.update(|cx| {
        notify_bell(SessionId(21), "wt-a", cx);
    });
    assert!(cx.shown_system_notifications().is_empty());
    cx.update(|cx| {
        sync_snapshot(
            &snapshot(vec![session(21, AgentState::Working, 3, "/repo/a")]),
            cx,
        );
        sync_snapshot(
            &snapshot(vec![session(21, AgentState::Done, 4, "/repo/a")]),
            cx,
        );
    });
    assert!(cx.shown_system_notifications().is_empty());

    // 토글을 켜면 새 전이(입력 대기)와 벨이 알린다.
    fs::write(
        dir.join("settings.toml"),
        "notifications = true\nnotify_on_input = true\nnotify_on_done = true\nnotify_on_bell = true\nkeep_awake = \"off\"\n",
    )
    .unwrap();
    cx.update(settings::reload);
    cx.run_until_parked();
    cx.update(|cx| {
        sync_snapshot(
            &snapshot(vec![session(21, AgentState::Working, 5, "/repo/a")]),
            cx,
        );
        sync_snapshot(
            &snapshot(vec![session(21, AgentState::Blocked, 6, "/repo/a")]),
            cx,
        );
        notify_bell(SessionId(22), "wt-b", cx);
    });
    assert_eq!(cx.shown_system_notifications().len(), 2);
    // 같은 세션에 이미 게시된 알림이 있으면 벨을 반복해도 늘지 않는다.
    cx.update(|cx| {
        notify_bell(SessionId(22), "wt-b", cx);
    });
    assert_eq!(cx.shown_system_notifications().len(), 2);

    // 테스트 알림은 기존 발송 경로로 나가고, 마스터를 끄면 나가지 않는다.
    cx.update(|cx| {
        send_test_notification(cx);
    });
    assert_eq!(cx.shown_system_notifications().len(), 3);
    fs::write(
        dir.join("settings.toml"),
        "notifications = false\nkeep_awake = \"off\"\n",
    )
    .unwrap();
    cx.update(settings::reload);
    cx.run_until_parked();
    cx.update(|cx| {
        send_test_notification(cx);
        notify_bell(SessionId(23), "wt-c", cx);
    });
    assert_eq!(cx.shown_system_notifications().len(), 3);
    fs::remove_dir_all(dir).ok();
}

/// OWE-659 회귀: 폴링 사이 Working 관측 없이 Waiting·Blocked→Done으로 가도 완료 알림이 나간다.
#[gpui::test]
fn done_notifies_from_waiting_and_blocked(cx: &mut TestAppContext) {
    let _guard = SERIAL
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let dir = temp_dir("owe-659-done");
    fs::write(
        dir.join("settings.toml"),
        "notifications = true\nnotify_on_input = false\nnotify_on_done = true\nnotify_on_bell = false\nkeep_awake = \"off\"\n",
    )
    .unwrap();
    init_settings(cx, &dir);
    cx.update(|cx| {
        sync_snapshot(
            &snapshot(vec![session(31, AgentState::Waiting, 1, "/repo/a")]),
            cx,
        );
    });
    // 입력 대기는 꺼져 있어 조용하다.
    assert!(cx.shown_system_notifications().is_empty());
    cx.update(|cx| {
        sync_snapshot(
            &snapshot(vec![session(31, AgentState::Done, 2, "/repo/a")]),
            cx,
        );
    });
    assert_eq!(cx.shown_system_notifications().len(), 1);
    // 같은 Done 반복은 다시 알리지 않는다.
    cx.update(|cx| {
        sync_snapshot(
            &snapshot(vec![session(31, AgentState::Done, 2, "/repo/a")]),
            cx,
        );
        sync_snapshot(
            &snapshot(vec![session(32, AgentState::Blocked, 3, "/repo/b")]),
            cx,
        );
        sync_snapshot(
            &snapshot(vec![
                session(31, AgentState::Done, 2, "/repo/a"),
                session(32, AgentState::Done, 4, "/repo/b"),
            ]),
            cx,
        );
    });
    assert_eq!(cx.shown_system_notifications().len(), 2);
    fs::remove_dir_all(dir).ok();
}

#[test]
fn notification_sound_truth_table() {
    let sounding = settings::Settings {
        notifications: true,
        notification_sound: true,
        ..settings::Settings::default()
    };
    assert!(should_play_sound(&sounding, 1));
    assert!(should_play_sound(&sounding, 3));
    assert!(!should_play_sound(&sounding, 0));
    let muted = settings::Settings {
        notification_sound: false,
        ..settings::Settings::default()
    };
    assert!(!should_play_sound(&muted, 2));
    let off = settings::Settings {
        notifications: false,
        ..settings::Settings::default()
    };
    assert!(!should_play_sound(&off, 2));
}

/// OWE-655: 새로 뜬 알림이 있을 때만 카운터가 오르고, 사운드를 끄면 알림은 뜨되 조용하다.
#[gpui::test]
fn notification_sound_gates_beep_count(cx: &mut TestAppContext) {
    let dir = temp_dir("sound");
    init_settings(cx, &dir);
    cx.update(|cx| {
        sync_snapshot(
            &snapshot(vec![session(9, AgentState::Working, 1, "/repo/a")]),
            cx,
        );
        assert_eq!(beep_count(cx), 0);
        sync_snapshot(
            &snapshot(vec![session(9, AgentState::Waiting, 2, "/repo/a")]),
            cx,
        );
        assert_eq!(beep_count(cx), 1);
    });
    assert_eq!(cx.shown_system_notifications().len(), 1);
    // 같은 알림 반복에는 추가 발음이 없다.
    cx.update(|cx| {
        sync_snapshot(
            &snapshot(vec![session(9, AgentState::Waiting, 2, "/repo/a")]),
            cx,
        );
        assert_eq!(beep_count(cx), 1);
    });
    // 사운드를 끄면 새 알림은 뜨지만 카운터는 그대로다.
    fs::write(
        dir.join("settings.toml"),
        "notifications = true\nnotification_sound = false\n",
    )
    .unwrap();
    cx.update(settings::reload);
    cx.run_until_parked();
    cx.update(|cx| {
        sync_snapshot(
            &snapshot(vec![session(9, AgentState::Blocked, 3, "/repo/a")]),
            cx,
        );
        assert_eq!(beep_count(cx), 1);
    });
    assert_eq!(cx.shown_system_notifications().len(), 2);
    fs::remove_dir_all(dir).ok();
}

#[gpui::test]
fn account_switch_history_notifies_once_after_initial_snapshot(cx: &mut TestAppContext) {
    let _guard = SERIAL
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let dir = temp_dir("account-switch");
    init_settings(cx, &dir);
    let record = |sequence| krill_proto::usage::SwitchRecord {
        sequence,
        at: 1000,
        event: krill_proto::usage::AutoSwitchEvent {
            provider: krill_proto::usage::ProviderKind::Claude,
            from_id: "a".into(),
            from_label: "Personal".into(),
            to_id: "b".into(),
            to_label: "Work".into(),
            reason_percent: 90,
        },
        predicted: false,
    };
    let state =
        |records: Vec<krill_proto::usage::SwitchRecord>| krill_proto::usage::AccountPolicyState {
            supported: true,
            history: records,
            ..Default::default()
        };
    cx.update(|cx| sync_account_switches(&state(vec![record(1)]), cx));
    assert!(cx.delivered_system_notifications().is_empty());
    cx.update(|cx| {
        // 데몬이 잠깐 안 받아 로컬 폴백(정책 꼬리 미지원, 빈 이력)이 와도 기준점을 0으로 되돌리지 않는다.
        sync_account_switches(&krill_proto::usage::AccountPolicyState::default(), cx);
        sync_account_switches(&state(vec![record(1), record(2)]), cx);
        sync_account_switches(&state(vec![record(1), record(2)]), cx);
    });
    assert_eq!(cx.delivered_system_notifications().len(), 1);
    fs::write(dir.join("settings.toml"), "notifications = false\n").unwrap();
    cx.update(settings::reload);
    cx.run_until_parked();
    cx.update(|cx| {
        sync_account_switches(&state(vec![record(2), record(3)]), cx);
    });
    assert_eq!(cx.delivered_system_notifications().len(), 1);
    fs::remove_dir_all(dir).ok();
}

#[gpui::test]
fn muted_sessions_worktrees_and_orchestration_workers_suppress_notifications(
    cx: &mut TestAppContext,
) {
    let _guard = SERIAL
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let dir = temp_dir("mute-workers");
    init_settings(cx, &dir);
    cx.update(|cx| {
        set_session_muted(SessionId(1), true, cx);
        set_worktree_muted("/repo/wt-muted", true, cx);
        set_orchestration_worker_sessions([SessionId(3)], cx);
        assert!(is_session_muted(SessionId(1), cx));
        assert!(is_worktree_muted(Path::new("/repo/wt-muted"), cx));
        sync_snapshot(
            &snapshot(vec![
                session(1, AgentState::Working, 1, "/repo/wt-a"),
                session(2, AgentState::Working, 1, "/repo/wt-muted"),
                session(3, AgentState::Working, 1, "/repo/wt-worker"),
                session(4, AgentState::Working, 1, "/repo/wt-normal"),
            ]),
            cx,
        );
        // 음소거된 세션(1)·워크트리(2)의 Waiting과 오케스트레이션 워커(3)의 Done은 억제되고, 일반 세션(4)의 Done만 발송된다.
        sync_snapshot(
            &snapshot(vec![
                session(1, AgentState::Waiting, 2, "/repo/wt-a"),
                session(2, AgentState::Blocked, 2, "/repo/wt-muted"),
                session(3, AgentState::Done, 2, "/repo/wt-worker"),
                session(4, AgentState::Done, 2, "/repo/wt-normal"),
            ]),
            cx,
        );
    });
    let delivered = cx.delivered_system_notifications();
    assert_eq!(delivered.len(), 1);
    assert_eq!(delivered[0].tag.as_ref(), "pane:4");
    assert_eq!(cx.read(dock_badge), None);
    fs::remove_dir_all(dir).ok();
}
