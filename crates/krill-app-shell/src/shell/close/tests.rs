use std::sync::mpsc;
use std::time::{Duration, Instant};

use gpui::TestAppContext;

use super::*;
use crate::shell::tests::{open, state_path};

#[gpui::test]
fn close_does_not_wait_for_creation_lock_and_keeps_late_cleanup(cx: &mut TestAppContext) {
    let (view, cx) = open(cx, state_path("close-contended-create"));
    let requests = Arc::new(std::sync::Mutex::new(Vec::new()));
    let creation = view.update(cx, |shell, _| {
        let recorded = requests.clone();
        shell.close_call = Some(Arc::new(move |request| {
            recorded.lock().unwrap().push(request.clone());
            Ok(Reply::Closed)
        }));
        let pane = shell.layout.active_tab().focused;
        let live = Live::new(Task::ready(()));
        Creation::publish(&live.creation, krill_proto::SessionId(112));
        let creation = live.creation.clone();
        shell.live.insert(pane, live);
        creation
    });
    let (started, ready) = mpsc::channel();
    let (release, wait) = mpsc::channel();
    let owner = std::thread::spawn(move || {
        let _guard = creation.lock().unwrap();
        started.send(()).unwrap();
        wait.recv_timeout(Duration::from_secs(2)).ok();
    });
    ready.recv_timeout(Duration::from_secs(2)).unwrap();
    let began = Instant::now();
    view.update(cx, |shell, cx| {
        let pane = shell.layout.active_tab().focused;
        shell.do_close_pane(pane, cx);
        shell.change(true, cx, Layout::new_tab);
    });
    let elapsed = began.elapsed();
    release.send(()).ok();
    owner.join().unwrap();
    assert!(
        elapsed < Duration::from_secs(1),
        "UI waited for creation lock: {elapsed:?}"
    );
    cx.run_until_parked();
    assert_eq!(
        *requests.lock().unwrap(),
        vec![Request::Close {
            session: krill_proto::SessionId(112)
        }]
    );
}

/// APP-RUNTIME-001: 발행된 세션 ID는 명시적으로 닫기 전까지 읽을 수 있고, 닫힌 뒤에는 돌려주지 않는다.
#[test]
fn published_id_is_readable_until_explicit_close() {
    let creation = std::sync::Mutex::new(Creation::default());
    assert_eq!(Creation::published(&creation), None);
    assert!(Creation::publish(&creation, krill_proto::SessionId(5)));
    assert_eq!(
        Creation::published(&creation),
        Some(krill_proto::SessionId(5))
    );
    assert_eq!(
        creation.lock().unwrap().close(),
        Some(krill_proto::SessionId(5))
    );
    assert_eq!(Creation::published(&creation), None);
    assert!(!Creation::publish(&creation, krill_proto::SessionId(6)));
}
