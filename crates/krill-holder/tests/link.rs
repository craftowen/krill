//! 전송로 계약: 스트림 소켓 + `SCM_RIGHTS`에서 fd가 자기 프레임에 붙어 오는지(macOS 실측).

#![expect(clippy::unwrap_used, reason = "테스트 헬퍼")]

use std::os::fd::{AsFd, OwnedFd};

use krill_holder::link::Link;
use krill_proto::holder::FromHolder;
use rustix::fs::fstat;
use rustix::net::{AddressFamily, SocketFlags, SocketType, socketpair};

fn pair() -> (Link, Link) {
    let (a, b) = socketpair(
        AddressFamily::UNIX,
        SocketType::STREAM,
        SocketFlags::empty(),
        None,
    )
    .unwrap();
    (Link::new(a), Link::new(b))
}

#[cfg_attr(
    target_os = "linux",
    expect(
        clippy::useless_conversion,
        reason = "Linux st_dev는 이미 u64다(macOS는 i32)"
    )
)]
fn ino(fd: impl AsFd) -> (u64, u64) {
    let st = fstat(fd).unwrap();
    (u64::try_from(st.st_dev).unwrap(), st.st_ino)
}

/// 계약: 프레임 수백 개를 읽기 전에 몰아 보내도 fd가 자기 프레임에 정확히 붙어 온다.
#[test]
fn fds_stay_with_their_frames() {
    let (tx, mut rx) = pair();
    let dir = std::env::temp_dir().join(format!("mkh-link-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let files: Vec<OwnedFd> = (0..300)
        .map(|i| std::fs::File::create(dir.join(i.to_string())).map(OwnedFd::from))
        .collect::<Result<_, _>>()
        .unwrap();
    let want: Vec<_> = files.iter().map(ino).collect();
    // 보내는 쪽은 받는 쪽보다 앞서 달린다(소켓 버퍼가 찰 때까지 여러 프레임이 쌓인다).
    let sender = std::thread::spawn(move || {
        for (i, f) in files.iter().enumerate() {
            let m = if i % 3 == 0 {
                FromHolder::Ready
            } else {
                FromHolder::Session {
                    id: krill_proto::SessionId(u64::try_from(i).unwrap()),
                    pid: 1,
                    status: None,
                }
            };
            let fds = if m.fds() == 1 {
                vec![f.as_fd()]
            } else {
                vec![]
            };
            tx.send(&m.encode().unwrap(), &fds).unwrap();
        }
    });
    std::thread::sleep(std::time::Duration::from_millis(50));
    let mut got = Vec::new();
    while let Some(p) = rx.recv().unwrap() {
        match FromHolder::decode(p.frame()).unwrap() {
            FromHolder::Session { id, .. } => {
                got.push((id.0, ino(&p.fds[0])));
            }
            FromHolder::Ready => assert!(p.fds.is_empty()),
            m => panic!("{m:?}"),
        }
    }
    sender.join().unwrap();
    std::fs::remove_dir_all(&dir).unwrap();
    assert_eq!(got.len(), 200);
    for (id, i) in got {
        assert_eq!(want[usize::try_from(id).unwrap()], i, "session {id}");
    }
}
