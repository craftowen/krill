use super::*;
use crate::holder::Holder;
use crate::os::OwnedFd;
use krill_proto::holder::VERSION;
use std::io::Write as _;
use std::sync::mpsc;

#[test]
fn peer_failure_before_first_master_keeps_existing_output_running() {
    const ROLE: &str = "KRILL_TEST_REJOIN_BEFORE_TRANSFER";
    if std::env::var_os(ROLE).is_none() {
        let dir = std::env::temp_dir().join(format!("krill-rejoin-before-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "rejoin::tests::peer_failure_before_first_master_keeps_existing_output_running",
                "--nocapture",
            ])
            .env(ROLE, "1")
            .env("KRILL_STATE_DIR", &dir)
            .env("HOME", &dir)
            .env("DO_NOT_TRACK", "1")
            .env_remove("CODEX_HOME")
            .env_remove("CLAUDE_CONFIG_DIR")
            .output()
            .unwrap();
        fs::remove_dir_all(dir).unwrap();
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    let (reg, mut process, _holder_peer) = fixture();
    let mut tap = None;
    reg.each(|_, _, _, session| tap = Some(session.tap()));
    let tap = tap.unwrap();
    process.write_all(b"before").unwrap();
    wait_for(|| tap.pulse().end == 6);
    let (giver, peer) = UnixStream::pair().unwrap();
    peer.shutdown(std::net::Shutdown::Both).unwrap();
    drop(peer);
    let failure = give(&reg, giver).unwrap_err();
    process.write_all(b"after").unwrap();
    let deadline = Instant::now() + Duration::from_secs(3);
    while tap.pulse().end != 11 {
        let mut paused = false;
        reg.each(|_, _, _, session| paused |= session.is_paused());
        assert!(
            Instant::now() < deadline,
            "pre-transfer failure {failure:?}: paused={paused}, pulse={:?}",
            tap.pulse()
        );
        thread::sleep(Duration::from_millis(1));
    }
    assert!(tap.screen_text_and_modes().0.contains("beforeafter"));
}

#[test]
fn missing_peer_and_incomplete_handover_have_different_retry_outcomes() {
    let dir = PathBuf::from(format!("/tmp/krill-take-retry-{}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    let socket = dir.join("daemon.sock");
    assert!(matches!(
        Handover::take(&socket),
        Err(TakeError::Unavailable(_))
    ));
    let listener = UnixListener::bind(handoff_path(&socket)).unwrap();
    let peer = thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        let link = Link::new(OwnedFd::from(stream));
        link.send(
            &FromHolder::Hello {
                version: VERSION,
                holder_pid: 1,
                inbox: false,
            }
            .encode()
            .unwrap(),
            &[],
        )
        .unwrap();
        // A reachable peer that closes before Ready is not a healthy duplicate.
    });
    assert!(matches!(
        Handover::take(&socket),
        Err(TakeError::Transfer(_))
    ));
    peer.join().unwrap();
    fs::remove_dir_all(dir).unwrap();
}

#[test]
fn peer_failure_after_master_transfer_does_not_resume_a_second_reader() {
    const ROLE: &str = "KRILL_TEST_REJOIN_AFTER_TRANSFER";
    if std::env::var_os(ROLE).is_none() {
        let dir = std::env::temp_dir().join(format!("krill-rejoin-after-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "rejoin::tests::peer_failure_after_master_transfer_does_not_resume_a_second_reader",
                "--nocapture",
            ])
            .env(ROLE, "1")
            .env("KRILL_STATE_DIR", &dir)
            .env("HOME", &dir)
            .env("DO_NOT_TRACK", "1")
            .env_remove("CODEX_HOME")
            .env_remove("CLAUDE_CONFIG_DIR")
            .output()
            .unwrap();
        fs::remove_dir_all(dir).unwrap();
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    let (reg, mut process, _holder_peer) = fixture();
    let mut tap = None;
    reg.each(|_, _, _, session| tap = Some(session.tap()));
    let tap = tap.unwrap();
    process.write_all(b"before").unwrap();
    wait_for(|| tap.pulse().end == 6);
    let (giver, peer) = UnixStream::pair().unwrap();
    let (transferred, received) = mpsc::channel();
    let peer = thread::spawn(move || {
        let mut peer = Link::new(OwnedFd::from(peer));
        let hello = peer.recv().unwrap().unwrap();
        assert!(matches!(
            holder::decode(hello).unwrap(),
            Some((FromHolder::Hello { .. }, _))
        ));
        let session = peer.recv().unwrap().unwrap();
        assert!(matches!(
            holder::decode(session).unwrap(),
            Some((FromHolder::Session { .. }, Some(_)))
        ));
        transferred.send(()).unwrap();
        // The master may already have been consumed; no acknowledgement follows.
    });
    assert!(give(&reg, giver).is_err());
    received.recv_timeout(Duration::from_secs(3)).unwrap();
    peer.join().unwrap();
    reg.each(|_, _, _, session| assert!(session.is_paused()));
    process.write_all(b"after").unwrap();
    thread::sleep(Duration::from_millis(50));
    assert_eq!(
        tap.pulse().end,
        6,
        "old reader resumed after an ambiguous transfer"
    );
}

#[expect(clippy::unwrap_used, reason = "isolated socket fixture setup")]
fn fixture() -> (Registry, UnixStream, Link) {
    let (daemon, peer) = UnixStream::pair().unwrap();
    let peer = Link::new(OwnedFd::from(peer));
    for message in [
        FromHolder::Hello {
            version: VERSION,
            holder_pid: 1,
            inbox: false,
        },
        FromHolder::Ready,
    ] {
        peer.send(&message.encode().unwrap(), &[]).unwrap();
    }
    let (holder, _reader, _, _) = Holder::connect(OwnedFd::from(daemon)).unwrap();
    let (master, process) = UnixStream::pair().unwrap();
    let reg = Registry::new(
        Some((
            holder,
            vec![Kept {
                id: SessionId(1),
                pid: std::process::id(),
                status: None,
                master: OwnedFd::from(master),
            }],
        )),
        None,
    );
    (reg, process, peer)
}

fn wait_for(mut condition: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(3);
    while !condition() {
        assert!(
            Instant::now() < deadline,
            "pre-transfer peer failure left the reader parked"
        );
        thread::sleep(Duration::from_millis(1));
    }
}
