use super::*;
use krill_proto::control::CaptureFormat;

const SIZE: WinSize = WinSize { cols: 80, rows: 24 };

#[test]
fn recorder_keyframes_survive_predecessor_gc_in_plain_and_encrypted_segments() {
    for encrypted in [false, true] {
        let root = std::env::temp_dir().join(format!(
            "mk024-live-keyframe-{encrypted}-{}",
            std::process::id()
        ));
        fs::remove_dir_all(&root).ok();
        privacy::prepare(&root.join("sessions")).unwrap();
        let key = encrypted.then(|| Arc::new(crypto::Key::from_slice(&[17; 32]).unwrap()));
        let store = Store::with_key(
            root.join("sessions"),
            Limits::default(),
            true,
            encrypted,
            key.clone(),
        );
        let id = SessionId(77);
        let look = no_theme();
        let mut engine = Engine::new(SIZE);
        let mut rec = store.recorder(id, SIZE).unwrap();
        let prefix = b"\x1b[31mretained-screen\r\n";
        engine.feed(prefix, Feed::Replay, &look);
        rec.append(prefix);
        rec.seal();
        assert!(rec.needs_keyframe());
        let snapshot = engine.capture_cli(CaptureFormat::Ansi).unwrap();
        rec.keyframe(snapshot.clone());
        let suffix = b"continued-screen\r\n\x1b[0m";
        engine.feed(suffix, Feed::Replay, &look);
        rec.append(suffix);
        rec.seal();

        let paths = segments(&store.session_dir(id)).unwrap();
        assert_eq!(paths.len(), 2);
        let disk = fs::read(&paths[1]).unwrap();
        assert_eq!(disk[7], segment::KEYFRAME_V1);
        let parsed = segment::read(&paths[1], key.as_deref()).unwrap().unwrap();
        assert_eq!(parsed.keyframe_bytes, snapshot);
        assert_eq!(parsed.records, vec![Record::Data(suffix.to_vec())]);
        assert!(parsed.sealed);
        assert_eq!(parsed.valid_len, disk.len() as u64);
        // 잘린 봉인 꼬리는 버리되 앞의 키프레임·원본 청크는 복구한다.
        for cut in disk.len() - 3..disk.len() {
            let partial = segment::decode(&disk[..cut], key.as_deref())
                .unwrap()
                .unwrap();
            assert_eq!(partial.keyframe_bytes, snapshot);
            assert_eq!(partial.records, parsed.records);
            assert!(!partial.sealed);
        }
        if encrypted {
            assert!(!disk.windows(15).any(|w| w == b"retained-screen"));
            assert!(segment::read(&paths[1], None).is_err());
        }
        fs::remove_file(&paths[0]).unwrap();
        let history = store.history(id).unwrap();
        let restored = history.restore().unwrap();
        assert_eq!(restored.screen().rows, engine.screen().rows);
        assert_eq!(restored.screen().cursor, engine.screen().cursor);
        assert_eq!(restored.end_seq, (prefix.len() + suffix.len()) as u64);
        // 원본 바이트 재전송에 ANSI 키프레임이 섞이면 안 된다.
        let mut raw = Vec::new();
        store
            .raw(id, prefix.len() as u64, restored.end_seq, |piece| {
                if let Piece::Data { bytes, .. } = piece {
                    raw.extend_from_slice(&bytes);
                }
                Ok(())
            })
            .unwrap();
        assert_eq!(raw, suffix);
        fs::remove_dir_all(root).unwrap();
    }
}

#[test]
fn keyframe_record_has_fixed_bytes_and_does_not_consume_seq() {
    let record = Record::Keyframe(b"x".to_vec());
    let mut bytes = Vec::new();
    record.encode_into(&mut bytes);
    assert_eq!(bytes, [4, 1, 0, 0, 0, b'x']);
    assert_eq!(record.seq_len(), 0);
}

#[test]
fn old_segment_fixture_still_restores_without_keyframes() {
    let root = std::env::temp_dir().join(format!("mk024-old-keyframe-{}", std::process::id()));
    fs::remove_dir_all(&root).ok();
    let store = Store::open(&root, Limits::default()).unwrap();
    let dir = store.session_dir(SessionId(42));
    mkdir(&dir).unwrap();
    fs::write(dir.join("seg-00000000000000000000.krs"), tests::SEGMENT_V1).unwrap();
    let restored = store.history(SessionId(42)).unwrap().restore().unwrap();
    assert_eq!(restored.end_seq, 12);
    assert!(restored.screen_text().contains("hi"));
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn live_pty_reader_records_start_keyframes_and_skips_incomplete_sequences() {
    for (tag, prefix, suffix) in [
        (
            "clean",
            &b"\x1b[31mretained"[..],
            &b"\x1b[2;1Hcontinued"[..],
        ),
        ("partial", &b"retained\x1b[31"[..], &b"mcontinued"[..]),
    ] {
        let root = std::env::temp_dir().join(format!("mk024-pty-{tag}-{}", std::process::id()));
        fs::remove_dir_all(&root).ok();
        let store = Store::open(
            &root,
            Limits {
                seal_bytes: 1,
                ..Limits::default()
            },
        )
        .unwrap();
        let id = SessionId(88);
        let pty = rustix_openpty::openpty(None, None).unwrap();
        let session = crate::Session::start(
            id,
            pty.controller,
            SIZE,
            Arc::new(no_theme()),
            Some(store.recorder(id, SIZE).unwrap()),
        )
        .unwrap();
        let tap = session.tap();
        let seed = b"earlier\x1b[2;1H";
        rustix::io::write(&pty.user, seed).unwrap();
        tap.wait(|p| p.end >= seed.len() as u64 || p.closed);
        assert_eq!(rustix::io::write(&pty.user, prefix).unwrap(), prefix.len());
        tap.wait(|p| p.end >= (seed.len() + prefix.len()) as u64 || p.closed);
        assert_eq!(rustix::io::write(&pty.user, suffix).unwrap(), suffix.len());
        tap.wait(|p| p.end >= (seed.len() + prefix.len() + suffix.len()) as u64 || p.closed);
        let before = tap.capture().0;
        drop(pty.user);
        session.wait().unwrap();
        let paths = segments(&store.session_dir(id)).unwrap();
        assert_eq!(paths.len(), 3);
        fs::remove_file(&paths[0]).unwrap();
        let last = segment::read(&paths[2], None).unwrap().unwrap();
        if tag == "clean" {
            assert!(!last.keyframe_bytes.is_empty());
            fs::remove_file(&paths[1]).unwrap();
        } else {
            assert!(last.keyframe_bytes.is_empty());
        }
        let after = store.history(id).unwrap().restore().unwrap().screen();
        assert_eq!(before.rows, after.rows, "{tag}");
        assert_eq!(before.cursor, after.cursor, "{tag}");
        fs::remove_dir_all(root).unwrap();
    }
}

#[test]
fn recording_failure_does_not_close_or_stop_the_live_pty_reader() {
    let root = std::env::temp_dir().join(format!("mk024-pty-write-fail-{}", std::process::id()));
    fs::remove_dir_all(&root).ok();
    let store = Store::open(
        &root,
        Limits {
            seal_bytes: 1,
            ..Limits::default()
        },
    )
    .unwrap();
    let id = SessionId(89);
    let pty = rustix_openpty::openpty(None, None).unwrap();
    let session = crate::Session::start(
        id,
        pty.controller,
        SIZE,
        Arc::new(no_theme()),
        Some(store.recorder(id, SIZE).unwrap()),
    )
    .unwrap();
    let tap = session.tap();
    rustix::io::write(&pty.user, b"before").unwrap();
    tap.wait(|p| p.end >= 6 || p.closed);
    let dir = store.session_dir(id);
    fs::remove_dir_all(&dir).unwrap();
    fs::write(&dir, b"prevent mkdir").unwrap();
    rustix::io::write(&pty.user, b" after").unwrap();
    let pulse = tap.wait(|p| p.end >= 12 || p.closed);
    assert!(!pulse.closed);
    assert!(session.screen_text().contains("before after"));
    drop(pty.user);
    session.wait().unwrap();
    fs::remove_dir_all(root).unwrap();
}

/// PTY가 OPOST ONLCR으로 `\n`을 `\r\n`으로 바꿔 돌려주므로 실제 받은 바이트 수를 센다.
fn received_len(bytes: &[u8]) -> usize {
    let mut extra = 0;
    for b in bytes {
        extra += usize::from(*b == b'\n');
    }
    bytes.len() + extra
}

#[test]
fn unsafe_ansi_boundaries_restore_original_scroll_regions_alt_screen_and_origin() {
    let cases: &[(&str, &[u8], &[u8])] = &[
        (
            "region",
            b"top\r\none\r\ntwo\r\nbot\x1b[2;3r\x1b[3;1H",
            b"\r\nnew",
        ),
        (
            "origin",
            b"top\r\none\r\ntwo\r\nbot\x1b[2;3r\x1b[?6h\x1b[2;1H",
            b"\r\nnew",
        ),
        ("alt", b"main\x1b[?1049halt", b"\x1b[?1049l again"),
        ("saved", b"main\x1b7\x1b[3;1Hother", b"\x1b8 again"),
        ("charset", b"\x1b(0lqk", b"\r\nxxx"),
    ];
    for &(tag, prefix, suffix) in cases {
        let root = std::env::temp_dir().join(format!("mk024-unsafe-{tag}-{}", std::process::id()));
        fs::remove_dir_all(&root).ok();
        let size = WinSize { cols: 20, rows: 4 };
        let store = Store::open(
            &root,
            Limits {
                seal_bytes: 1,
                ..Limits::default()
            },
        )
        .unwrap();
        let id = SessionId(90);
        let pty = rustix_openpty::openpty(None, None).unwrap();
        let session = crate::Session::start(
            id,
            pty.controller,
            size,
            Arc::new(no_theme()),
            Some(store.recorder(id, size).unwrap()),
        )
        .unwrap();
        let tap = session.tap();
        rustix::io::write(&pty.user, prefix).unwrap();
        // OPOST의 LF 변환까지 모두 받은 다음에 다음 세그먼트로 간다.
        let count = received_len(prefix);
        tap.wait(|p| p.end >= count as u64 || p.closed);
        rustix::io::write(&pty.user, suffix).unwrap();
        let count = count + received_len(suffix);
        tap.wait(|p| p.end >= count as u64 || p.closed);
        let before = tap.capture().0;
        drop(pty.user);
        session.wait().unwrap();
        let paths = segments(&store.session_dir(id)).unwrap();
        // PTY 한 번 쓰기가 여러 번 읽히면 prefix만으로도 세그먼트가 나뉜다.
        assert!(paths.len() >= 2, "{tag}: {paths:?}");
        let last = segment::read(paths.last().unwrap(), None).unwrap().unwrap();
        assert!(last.keyframe_bytes.is_empty(), "{tag}");
        let after = store.history(id).unwrap().restore().unwrap().screen();
        assert_eq!(before.rows, after.rows, "{tag}");
        assert_eq!(before.cursor, after.cursor, "{tag}");
        fs::remove_dir_all(root).unwrap();
    }
}

#[test]
fn paused_recording_writes_zero_bytes_and_resumes_with_gap_without_private_snapshots() {
    let root = std::env::temp_dir().join(format!("mk024-pause-{}", std::process::id()));
    fs::remove_dir_all(&root).ok();
    let store = Store::open(&root, Limits::default()).unwrap();
    let id = SessionId(91);
    let mut rec = store.recorder(id, SIZE).unwrap();
    rec.append(b"public before");
    let path = segments(&store.session_dir(id)).unwrap().remove(0);
    let before = fs::read(&path).unwrap();
    let written = store.written();
    store.set_paused(true);
    rec.append(b"secret while paused");
    rec.resize(WinSize {
        cols: 100,
        rows: 30,
    });
    rec.seal();
    assert_eq!(store.written(), written);
    assert_eq!(fs::read(&path).unwrap(), before);
    assert_eq!(segments(&store.session_dir(id)).unwrap().len(), 1);
    store.set_paused(false);
    rec.keyframe(b"secret still visible on screen".to_vec());
    rec.append(b" public after");
    rec.seal();
    let paths = segments(&store.session_dir(id)).unwrap();
    assert_eq!(paths.len(), 2);
    let last = segment::read(&paths[1], None).unwrap().unwrap();
    assert!(last.keyframe_bytes.is_empty());
    assert!(last.records.contains(&Record::Gap(19)));
    assert!(last.records.iter().all(|r| match r {
        Record::Data(bytes) | Record::Keyframe(bytes) => !bytes.windows(6).any(|w| w == b"secret"),
        _ => true,
    }));
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn stored_pause_is_applied_at_startup_and_missing_setting_defaults_to_recording() {
    let root = std::env::temp_dir().join(format!("mk024-pause-start-{}", std::process::id()));
    fs::remove_dir_all(&root).ok();
    fs::create_dir_all(&root).unwrap();
    fs::write(
        root.join("settings.toml"),
        "recording_paused = true\nlanguage = 'ko'\n",
    )
    .unwrap();
    let store = Store::open(&root, Limits::default()).unwrap();
    let mut rec = store.recorder(SessionId(92), SIZE).unwrap();
    rec.append(b"private");
    rec.seal();
    assert_eq!(store.written(), 0);
    assert!(store.history(SessionId(92)).unwrap().is_empty());
    fs::remove_file(root.join("settings.toml")).unwrap();
    let store = Store::open(&root, Limits::default()).unwrap();
    let mut rec = store.recorder(SessionId(92), SIZE).unwrap();
    rec.append(b"public");
    rec.seal();
    assert!(store.written() > 0);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn pause_open_and_append_share_one_lock_order_without_deadlock() {
    let root = std::env::temp_dir().join(format!("mk024-pause-lock-{}", std::process::id()));
    fs::remove_dir_all(&root).ok();
    let store = Store::open(
        &root,
        Limits {
            seal_bytes: 1,
            ..Limits::default()
        },
    )
    .unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    let mut threads = Vec::new();
    for worker in 0..3 {
        let store = Arc::clone(&store);
        let tx = tx.clone();
        threads.push(std::thread::spawn(move || {
            for round in 0..100 {
                match worker {
                    0 => store.set_paused(round % 2 == 0),
                    1 => {
                        let mut rec = store.recorder(SessionId(100 + round), SIZE).unwrap();
                        rec.append(b"open");
                        rec.seal();
                    }
                    _ => {
                        let mut rec = store.recorder(SessionId(300 + round), SIZE).unwrap();
                        for _ in 0..4 {
                            rec.append(b"append");
                        }
                        rec.seal();
                    }
                }
            }
            tx.send(()).unwrap();
        }));
    }
    for _ in 0..3 {
        rx.recv_timeout(Duration::from_secs(15))
            .expect("recording lock deadlock");
    }
    for thread in threads {
        thread.join().unwrap();
    }
    fs::remove_dir_all(root).unwrap();
}
