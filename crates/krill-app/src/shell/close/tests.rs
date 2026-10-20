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
