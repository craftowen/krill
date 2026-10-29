use super::*;
use std::io::Write as _;

type CloseGate = (
    SessionId,
    std::sync::mpsc::Sender<()>,
    std::sync::mpsc::Receiver<()>,
);
static BEFORE_CLOSE: Mutex<Option<CloseGate>> = Mutex::new(None);
static BEFORE_RESUME: Mutex<Option<CloseGate>> = Mutex::new(None);

pub(super) fn before_close(id: SessionId) {
    wait_gate(id, &BEFORE_CLOSE);
}

pub(super) fn before_resume(id: SessionId) {
    wait_gate(id, &BEFORE_RESUME);
}

fn wait_gate(id: SessionId, slot: &Mutex<Option<CloseGate>>) {
    let gate = {
        let mut gate = lock(slot);
        if gate.as_ref().is_some_and(|(target, _, _)| *target == id) {
            gate.take()
        } else {
            None
        }
    };
    if let Some((_, arrived, release)) = gate {
        arrived.send(()).unwrap();
        release.recv().unwrap();
    }
}

#[test]
fn aborted_upgrade_restores_existing_reader_and_new_session_output() {
    const ROLE: &str = "KRILL_TEST_ABORTED_UPGRADE";
    if std::env::var_os(ROLE).is_none() {
        let dir =
            std::env::temp_dir().join(format!("krill-aborted-upgrade-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "session::pause_tests::aborted_upgrade_restores_existing_reader_and_new_session_output", "--nocapture"])
            .env(ROLE, "1").env("KRILL_STATE_DIR", &dir).env("HOME", &dir).env("USERPROFILE", &dir)
            .env("LOCALAPPDATA", dir.join("Local")).env("APPDATA", dir.join("Roaming"))
            .output().unwrap();
        std::fs::remove_dir_all(dir).unwrap();
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    let root =
        std::path::PathBuf::from(std::env::var_os("KRILL_STATE_DIR").unwrap()).join("recorded");
    let store = Store::open(&root, crate::store::Limits::default()).unwrap();
    let (session, mut output) = fixture(1, Some(&store));
    let tap = session.tap();
    output.write_all(b"before\x1b[").unwrap();
    wait_for(|| tap.pulse().end == 8);
    let reg = crate::registry::Registry::new(None, None);
    {
        let _paused = crate::rejoin::halt_all(&reg).unwrap();
        wait_for(|| {
            crate::session::wake_halt();
            session.is_paused()
        });
        // Returning an upgrade error drops this preparation without exiting the daemon.
    }
    output.write_all(b"31mafter\r\n").unwrap();
    wait_for(|| tap.pulse().end == 18);
    assert!(tap.screen_text_and_modes().0.contains("beforeafter"));
    assert!(
        !tap.pulse().closed,
        "handoff pause was published as terminal EOF"
    );
    for expected in 19..=20 {
        {
            let _paused = crate::rejoin::halt_all(&reg).unwrap();
            wait_for(|| {
                crate::session::wake_halt();
                session.is_paused()
            });
        }
        output.write_all(b".").unwrap();
        wait_for(|| tap.pulse().end == expected);
    }
    // A process-wide shutdown latch must not silently kill newly created readers either.
    let (second, mut second_output) = fixture(2, None);
    second_output.write_all(b"new-session").unwrap();
    wait_for(|| second.tap().pulse().end == 11);
    drop((output, second_output));
    session.wait().unwrap();
    second.wait().unwrap();
    let mut replay = Vec::new();
    store
        .raw(SessionId(1), 0, 20, |piece| {
            match piece {
                Piece::Data { bytes, .. } => replay.extend(bytes),
                Piece::Gap { .. } => panic!("aborted upgrade lost recorded output"),
            }
            Ok(())
        })
        .unwrap();
    assert_eq!(replay, b"before\x1b[31mafter\r\n..");
    let restored = store.history(SessionId(1)).unwrap().restore().unwrap();
    assert_eq!(restored.end_seq, 20);
    assert!(restored.screen_text().contains("beforeafter"));

    // Exercise Windows' bounded read-ahead and cancellation with output still in flight.
    let (flood, mut flood_output) = fixture(3, Some(&store));
    let flood_tap = flood.tap();
    let bytes = vec![b'q'; 512 * 1024];
    let sent = bytes.clone();
    let writer = thread::spawn(move || flood_output.write_all(&sent).unwrap());
    wait_for(|| flood_tap.pulse().end > 0);
    {
        let _paused = crate::rejoin::halt_all(&reg).unwrap();
        wait_for(|| {
            crate::session::wake_halt();
            flood.is_paused() || flood.is_closed()
        });
    }
    writer.join().unwrap();
    flood.wait().unwrap();
    let mut replay = Vec::new();
    store
        .raw(SessionId(3), 0, bytes.len() as u64, |piece| {
            match piece {
                Piece::Data { bytes, .. } => replay.extend(bytes),
                Piece::Gap { .. } => panic!("aborted upgrade lost queued output"),
            }
            Ok(())
        })
        .unwrap();
    assert_eq!(replay, bytes, "handoff duplicated or lost queued bytes");

    // The read loop has already selected real EOF. Starting preparation before
    // final publication must not suppress that EOF, even if preparation aborts.
    let (ended, mut output) = fixture(4, Some(&store));
    let tap = ended.tap();
    output.write_all(b"ended").unwrap();
    wait_for(|| tap.pulse().end == 5);
    let (arrived, at_close) = std::sync::mpsc::channel();
    let (release, proceed) = std::sync::mpsc::channel();
    *lock(&BEFORE_CLOSE) = Some((SessionId(4), arrived, proceed));
    drop(output);
    at_close.recv_timeout(Duration::from_secs(3)).unwrap();
    let preparation = Handoff::begin().unwrap();
    release.send(()).unwrap();
    ended.wait().unwrap();
    drop(preparation);
    assert!(tap.pulse().closed, "handoff suppressed a real terminal EOF");

    // Hold a real reader between observing rollback and clearing its paused bit.
    // Another preparation must not accept that bit as an acknowledgement of its pause.
    let (reader, mut output) = fixture(5, None);
    let preparation = Handoff::begin().unwrap();
    wait_for(|| {
        wake_halt();
        reader.is_paused()
    });
    let (arrived, resuming) = std::sync::mpsc::channel();
    let (release, proceed) = std::sync::mpsc::channel();
    *lock(&BEFORE_RESUME) = Some((SessionId(5), arrived, proceed));
    drop(preparation);
    resuming.recv_timeout(Duration::from_secs(3)).unwrap();
    let (attempting, attempt) = std::sync::mpsc::channel();
    let (acquired, acquisition) = std::sync::mpsc::channel();
    let (finish, finishing) = std::sync::mpsc::channel();
    let next = thread::spawn(move || {
        attempting.send(()).unwrap();
        let _preparation = Handoff::begin().unwrap();
        acquired.send(()).unwrap();
        finishing.recv().unwrap();
    });
    attempt.recv_timeout(Duration::from_secs(3)).unwrap();
    let stale_pause_accepted = acquisition.recv_timeout(Duration::from_millis(100)).is_ok();
    release.send(()).unwrap();
    if !stale_pause_accepted {
        acquisition.recv_timeout(Duration::from_secs(3)).unwrap();
    }
    finish.send(()).unwrap();
    next.join().unwrap();
    output.write_all(b"resumed").unwrap();
    wait_for(|| reader.tap().pulse().end == 7);
    drop(output);
    reader.wait().unwrap();
    assert!(
        !stale_pause_accepted,
        "next handoff accepted a stale pause while the old reader was resuming"
    );

    // Rollback need not wait for a pause acknowledgement. A canceled worker
    // still in ReadFile must drain and retire before a replacement can read.
    let (early, mut output) = fixture(7, Some(&store));
    drop(Handoff::begin().unwrap());
    let expected = b"early-rollback\r\n".repeat(8192);
    output.write_all(&expected).unwrap();
    drop(output);
    early.wait().unwrap();
    let mut replay = Vec::new();
    store
        .raw(SessionId(7), 0, expected.len() as u64, |piece| {
            match piece {
                Piece::Data { bytes, .. } => replay.extend(bytes),
                Piece::Gap { .. } => panic!("early rollback lost output"),
            }
            Ok(())
        })
        .unwrap();
    assert_eq!(replay, expected, "early rollback duplicated or lost output");

    // A failed retry must not undo an already committed orphan handoff: its peer
    // might already be consuming the transferred PTY. This is the final case in
    // the isolated child, whose committed readers intentionally stay parked.
    let (parked, _output) = fixture(6, None);
    let preparation = Handoff::begin().unwrap();
    wait_for(|| {
        wake_halt();
        parked.is_paused()
    });
    preparation.commit();
    drop(Handoff::begin().unwrap());
    assert!(
        HALTED.load(Ordering::SeqCst),
        "aborted retry resumed a previously committed handoff"
    );
}

fn wait_for(mut condition: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(3);
    while !condition() {
        assert!(
            Instant::now() < deadline,
            "aborted update left the PTY reader permanently stopped"
        );
        thread::sleep(Duration::from_millis(1));
    }
}

fn fixture(id: u64, store: Option<&Arc<Store>>) -> (Session, impl std::io::Write + Send + 'static) {
    #[cfg(unix)]
    let (master, output) = {
        let (master, output) = krill_proto::os::net::UnixStream::pair().unwrap();
        (OwnedFd::from(master), output)
    };
    #[cfg(windows)]
    let (master, output) = {
        use std::os::windows::io::{AsHandle as _, OwnedHandle};
        let (reader, output) = std::io::pipe().unwrap();
        let (_, input) = std::io::pipe().unwrap();
        let signal = reader.as_handle().try_clone_to_owned().unwrap();
        let pty = crate::holder::Master::from_handles(vec![
            OwnedHandle::from(input),
            OwnedHandle::from(reader),
            signal,
        ])
        .unwrap();
        (pty, output)
    };
    let session = Session::start(
        SessionId(id),
        master,
        #[cfg(windows)]
        None,
        WinSize { cols: 80, rows: 24 },
        Arc::new(Appearance::load(std::path::PathBuf::new())),
        store.map(|store| {
            store
                .recorder(SessionId(id), WinSize { cols: 80, rows: 24 })
                .unwrap()
        }),
    )
    .unwrap();
    (session, output)
}
