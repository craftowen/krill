//! Deterministically lose the holder after `UpgradeKrilld` Done and reader pause.
#![expect(
    clippy::unwrap_used,
    reason = "isolated fault-injection fixture and channel gates"
)]

use super::*;
use crate::holder::{Kept, link::Link};
use krill_proto::SessionId;
use krill_proto::holder::VERSION;
use std::io::{Read as _, Write as _};
use std::os::unix::net::UnixStream;
use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use std::time::{Duration, Instant};

type Gate = (mpsc::Sender<()>, mpsc::Receiver<()>);
static AFTER_PAUSE: Mutex<Option<Gate>> = Mutex::new(None);

pub(super) fn after_pause() {
    let gate = AFTER_PAUSE.lock().unwrap().take();
    if let Some((arrived, release)) = gate {
        arrived.send(()).unwrap();
        release.recv_timeout(Duration::from_secs(5)).unwrap();
    }
}

#[test]
fn holder_loss_after_pause_restores_existing_reader_and_input() {
    const ROLE: &str = "KRILL_TEST_UPGRADE_PAUSE_LOSS";
    if std::env::var_os(ROLE).is_none() {
        let dir = std::env::temp_dir().join(format!("krill-upgrade-pause-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "upgrade::tests::holder_loss_after_pause_restores_existing_reader_and_input",
                "--nocapture",
            ])
            .env(ROLE, "1")
            .env("HOME", &dir)
            .env("KRILL_STATE_DIR", &dir)
            .env("KRILL_LOG_DIR", dir.join("logs"))
            .env_remove("CLAUDE_CONFIG_DIR")
            .env_remove("CODEX_HOME")
            .output()
            .unwrap();
        std::fs::remove_dir_all(dir).unwrap();
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }

    let (daemon, peer) = UnixStream::pair().unwrap();
    let mut peer = Link::new(peer.into());
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
    let (holder, reader, _, _) = Holder::connect(daemon.into()).unwrap();
    let (master, mut process) = UnixStream::pair().unwrap();
    process
        .set_read_timeout(Some(Duration::from_secs(3)))
        .unwrap();
    let reg = Arc::new(Registry::new(
        Some((
            holder,
            vec![Kept {
                id: SessionId(1),
                pid: std::process::id(),
                status: None,
                master: master.into(),
            }],
        )),
        None,
    ));
    let holder = reg.holder().unwrap();
    let running = Arc::clone(&reg);
    let reader = thread::spawn(move || running.holder().unwrap().run(reader, |_, _| {}));
    let (close, closed) = mpsc::channel();
    let fake = thread::spawn(move || {
        for phase in 0..2 {
            let packet = peer.recv().unwrap().unwrap();
            let req = match ToHolder::decode(packet.frame()).unwrap() {
                ToHolder::UpgradeHolder { req, .. } if phase == 0 => req,
                ToHolder::UpgradeKrilld { req, .. } if phase == 1 => req,
                other => panic!("unexpected upgrade phase: {other:?}"),
            };
            peer.send(&FromHolder::Done { req }.encode().unwrap(), &[])
                .unwrap();
        }
        closed.recv_timeout(Duration::from_secs(5)).unwrap();
    });
    let mut tap = None;
    reg.each(|_, _, _, session| tap = Some(session.tap()));
    let tap = tap.unwrap();
    process.write_all(b"before").unwrap();
    wait_for(|| tap.pulse().end == 6);
    let (arrived, paused) = mpsc::channel();
    let (release, resume) = mpsc::channel();
    *AFTER_PAUSE.lock().unwrap() = Some((arrived, resume));
    let upgrading = Arc::clone(&reg);
    let upgrade = thread::spawn(move || {
        prepare(
            &upgrading,
            upgrading.holder().unwrap(),
            Upgrade {
                build: "999.0.0".to_owned(),
                holder: b"/usr/bin/true".to_vec(),
                krilld: b"/usr/bin/true".to_vec(),
            },
        )
    });
    paused.recv_timeout(Duration::from_secs(5)).unwrap();
    reg.each(|_, _, _, session| assert!(session.is_paused()));
    close.send(()).unwrap();
    fake.join().unwrap();
    reader.join().unwrap();
    assert!(!holder.is_connected());
    release.send(()).unwrap();
    assert_eq!(upgrade.join().unwrap(), Err(Failure::NoHolder));
    process.write_all(b"after").unwrap();
    wait_for(|| tap.pulse().end == 11);
    assert!(tap.screen_text_and_modes().0.contains("beforeafter"));
    tap.write_input(b"fresh-input", None).unwrap();
    let mut input = [0; 11];
    process.read_exact(&mut input).unwrap();
    assert_eq!(&input, b"fresh-input");
    drop(process);
    wait_for(|| tap.pulse().closed);
}

fn wait_for(mut condition: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(3);
    while !condition() {
        assert!(Instant::now() < deadline, "upgrade failure left IO parked");
        thread::sleep(Duration::from_millis(1));
    }
}
