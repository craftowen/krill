//! OWE-589: Exited가 exec 응답보다 먼저 와도 holder 리더가 Release 송신에 막히지 않는다.
#![cfg(unix)]
use std::os::fd::OwnedFd;
use std::os::unix::net::UnixStream;
use std::sync::{Arc, mpsc};
use std::thread;
use std::time::Duration;

use krill_holder::link::Link;
use krill_proto::SessionId;
use krill_proto::holder::{FromHolder, ToHolder, VERSION};
use krilld::holder::Holder;

#[test]
fn exited_release_during_holder_upgrade_does_not_block_reader() {
    let (ours, theirs) = UnixStream::pair().unwrap();
    let mut peer = Link::new(OwnedFd::from(theirs));
    for m in [
        FromHolder::Hello {
            version: VERSION,
            holder_pid: 1,
        },
        FromHolder::Ready,
    ] {
        peer.send(&m.encode().unwrap(), &[]).unwrap();
    }
    let (holder, link, _, _) = Holder::connect(OwnedFd::from(ours)).unwrap();
    let holder = Arc::new(holder);
    let (released, done) = mpsc::channel();
    let reader = Arc::clone(&holder);
    let reading = thread::spawn(move || {
        reader.run(link, |id, _| {
            reader.notify(|req| ToHolder::Release { req, id }).unwrap();
            released.send(()).unwrap();
        });
    });
    let upgrading = Arc::clone(&holder);
    let (finished, result) = mpsc::channel();
    let upgrade = thread::spawn(move || {
        finished
            .send(upgrading.upgrade_holder(b"/holder".to_vec()).is_ok())
            .unwrap();
    });
    let packet = peer.recv().unwrap().unwrap();
    let ToHolder::UpgradeHolder { req, .. } = ToHolder::decode(packet.frame()).unwrap() else {
        panic!("upgrade")
    };
    peer.send(
        &FromHolder::Exited {
            id: SessionId(7),
            status: 0,
        }
        .encode()
        .unwrap(),
        &[],
    )
    .unwrap();
    // exec가 완료되기 전에는 Release를 링크에 쓰면 안 되지만, 리더는 Done을 읽을 수 있어야 한다.
    peer.send(&FromHolder::Done { req }.encode().unwrap(), &[])
        .unwrap();
    assert!(
        result.recv_timeout(Duration::from_secs(2)).unwrap(),
        "upgrade failed"
    );
    done.recv_timeout(Duration::from_secs(2)).unwrap();
    let packet = peer.recv().unwrap().unwrap();
    assert!(matches!(
        ToHolder::decode(packet.frame()).unwrap(),
        ToHolder::Release {
            id: SessionId(7),
            ..
        }
    ));
    drop(peer);
    upgrade.join().unwrap();
    reading.join().unwrap();
}

/// K-06: holder-events 워커는 종료 처리 panic 뒤에도 살아남는다.
/// 수정 전에는 첫 `Exited` 처리 panic으로 워커가 죽어 이후 `Exited`가 버려지고
/// closing 세션이 Release되지 않는다.
#[test]
fn exited_handler_panic_does_not_stop_worker() {
    let (ours, theirs) = UnixStream::pair().unwrap();
    let peer = Link::new(OwnedFd::from(theirs));
    for m in [
        FromHolder::Hello {
            version: VERSION,
            holder_pid: 1,
        },
        FromHolder::Ready,
    ] {
        peer.send(&m.encode().unwrap(), &[]).unwrap();
    }
    let (holder, link, _, _) = Holder::connect(OwnedFd::from(ours)).unwrap();
    let (seen, seen_rx) = mpsc::channel();
    let reading = thread::spawn(move || {
        holder.run(link, |id, status| {
            assert_ne!(id, SessionId(1), "injected exited-handler panic");
            seen.send((id, status)).unwrap();
        });
    });
    for (id, status) in [(SessionId(1), 0), (SessionId(2), 3)] {
        peer.send(&FromHolder::Exited { id, status }.encode().unwrap(), &[])
            .unwrap();
    }
    // 첫 처리가 panic해도 둘째는 처리돼야 한다.
    assert_eq!(
        seen_rx.recv_timeout(Duration::from_secs(5)).unwrap(),
        (SessionId(2), 3)
    );
    drop(peer);
    reading.join().unwrap();
}
