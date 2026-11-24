//! 앱의 krilld 교체와 칸의 세션 생성 요청이 겹치지 않는다(Windows 실기: 앱 시작 직후 `resume_cold`가
//! 교체 중인 구 krilld에서 `ConnectionReset` 10054로 끊겨 "Could not confirm whether the new session was created").

use std::sync::atomic::{AtomicBool, Ordering};

use krill_proto::os::net::UnixListener;

use super::*;

#[test]
fn session_requests_wait_for_a_running_daemon_upgrade() {
    let dir = std::env::temp_dir().join(format!("krill-upgrade-gate-{}", std::process::id()));
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(&dir).unwrap();
    let paths = Paths {
        socket: dir.join("k.sock"),
        holder: dir.join("holder"),
        krilld: dir.join("daemon"),
    };
    let listener = UnixListener::bind(&paths.socket).unwrap();
    let upgraded = Arc::new(AtomicBool::new(false));
    let early = Arc::new(AtomicBool::new(false));
    let (seen, connected_early) = (Arc::clone(&upgraded), Arc::clone(&early));
    // One thread per connection, like krilld. The client's probe connection closes before the
    // real one, and Windows can deliver that EOF late. A serial server would never accept the next one.
    thread::spawn(move || {
        for s in listener.incoming() {
            let Ok(mut s) = s else { return };
            // 교체가 끝나기 전에 붙으면 구 krilld가 받았을 연결이다.
            if !seen.load(Ordering::SeqCst) {
                connected_early.store(true, Ordering::SeqCst);
            }
            thread::spawn(move || {
                let hello = ipc::ServerHello {
                    proto_min: 1,
                    proto_max: 1,
                    features: FEAT_CONTROL | FEAT_RESUME,
                    build: "test".into(),
                    instance_id: 2,
                };
                s.write_all(&hello.encode().unwrap()).unwrap();
                ipc::read_frame(&mut s).unwrap().unwrap();
                let Ok(Some((k, b))) = ipc::read_frame(&mut s) else {
                    return;
                };
                assert!(matches!(
                    Request::decode(k, &b).unwrap(),
                    Request::Resume { .. }
                ));
                let reply = Reply::Resumed {
                    session: SessionId(8),
                    pid: 1,
                };
                s.write_all(&reply.encode().unwrap()).unwrap();
            });
        }
    });
    let (started_tx, started) = mpsc::channel();
    let flag = Arc::clone(&upgraded);
    let upgrade = thread::spawn(move || {
        let _gate = DAEMON_UPGRADE.write().unwrap();
        started_tx.send(()).unwrap();
        thread::sleep(Duration::from_millis(300));
        flag.store(true, Ordering::SeqCst);
    });
    started.recv().unwrap();
    let create = Create {
        account: None,
        program: Program::Login,
        cwd: Vec::new(),
        cols: 80,
        rows: 24,
    };
    assert_eq!(
        resume_session_in(&paths, Some(SessionId(7)), &create).unwrap(),
        SessionId(8)
    );
    upgrade.join().unwrap();
    assert!(
        !early.load(Ordering::SeqCst),
        "connected during the upgrade"
    );
    std::fs::remove_dir_all(&dir).ok();
}
