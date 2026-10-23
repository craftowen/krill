use std::path::PathBuf;
use std::sync::Arc;

use super::*;
use crate::os::OwnedFd;
use crate::{Appearance, Session};

const DESKTOP: WinSize = WinSize { cols: 80, rows: 24 };
const OTHER: WinSize = WinSize {
    cols: 100,
    rows: 40,
};

#[expect(
    clippy::unwrap_used,
    reason = "실패하면 중단하는 테스트 PTY 초기화 헬퍼"
)]
fn fixture() -> (OwnedFd, Session) {
    let size = rustix::termios::Winsize {
        ws_col: DESKTOP.cols,
        ws_row: DESKTOP.rows,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    let pty = rustix_openpty::openpty(None, Some(&size)).unwrap();
    let session = Session::start(
        krill_proto::SessionId(702),
        pty.controller,
        DESKTOP,
        Arc::new(Appearance::load(PathBuf::new())),
        None,
    )
    .unwrap();
    (pty.user, session)
}

#[test]
fn resize_failure_invalidates_cache_before_focus_or_disconnect_restores_owner() {
    for disconnect in [false, true] {
        let (slave, session) = fixture();
        let tap = session.tap();
        let first = Client::new(tap.clone(), Role::Interactive).unwrap();
        assert_eq!(first.request(Request::Focus), Ack::Applied);
        let mut second = Some(Client::new(tap.clone(), Role::Interactive).unwrap());
        let client = second.as_ref().unwrap();
        assert_eq!(
            client.request(Request::Resize {
                cols: OTHER.cols,
                rows: OTHER.rows
            }),
            Ack::Applied
        );
        {
            let mut clients = tap.clients().lock().unwrap();
            assert_eq!(clients.applied, Some(DESKTOP));
            clients
                .entries
                .iter_mut()
                .find(|e| e.id == client.id)
                .unwrap()
                .active = true;
            // 엔진 복구 오류처럼 실제 크기가 바뀐 뒤 실패한다.
            assert!(
                clients
                    .apply_with(&tap, |tap, size| {
                        tap.resize(size)?;
                        Err(io::Error::other("failure after resize"))
                    })
                    .is_err()
            );
            assert_eq!(clients.applied, None);
        }
        assert_eq!(tap.size().unwrap(), OTHER);
        if disconnect {
            drop(second.take());
        } else {
            assert_eq!(first.request(Request::Focus), Ack::Applied);
        }
        assert_eq!(tap.size().unwrap(), DESKTOP);
        assert_eq!(tap.clients().lock().unwrap().applied, Some(DESKTOP));
        drop(second);
        drop(first);
        drop(slave);
        session.wait().unwrap();
    }
}

#[test]
fn failed_mobile_fallback_is_retained_for_the_next_resize_attempt() {
    let (slave, session) = fixture();
    let tap = session.tap();
    let mobile = Client::new(tap.clone(), Role::InteractiveWithSize { owns_size: false }).unwrap();
    assert_eq!(
        mobile.request(Request::Resize {
            cols: OTHER.cols,
            rows: OTHER.rows
        }),
        Ack::Applied
    );
    {
        let mut clients = tap.clients().lock().unwrap();
        assert_eq!(clients.mobile_fallback, Some(DESKTOP));
        clients.entries.clear();
        assert!(
            clients
                .apply_with(&tap, |_, _| Err(io::Error::other("OS resize failure")))
                .is_err()
        );
        assert_eq!(clients.applied, None);
        assert_eq!(clients.mobile_fallback, Some(DESKTOP));
    }
    assert_eq!(tap.size().unwrap(), OTHER);
    drop(mobile);
    assert_eq!(tap.size().unwrap(), DESKTOP);
    let clients = tap.clients().lock().unwrap();
    assert_eq!(clients.mobile_fallback, None);
    assert_eq!(clients.applied, Some(DESKTOP));
    drop(clients);
    drop(slave);
    session.wait().unwrap();
}
