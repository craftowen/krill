//! MK-097 암호화·키체인·성능 회귀. 키체인 IO는 `HOME/CFFIXED_USER_HOME` 오버레이 자식에 격리한다.

use super::crypto::{Key, SALT_LEN, Stream};
use super::segment::{HEADER_LEN, Header};
use super::tests::{SEGMENT_V1, noise};
use super::*;
#[cfg(target_os = "macos")]
use security_framework::os::macos::keychain::{CreateOptions, SecKeychain};
use std::process::Command;

const SIZE: WinSize = WinSize { cols: 80, rows: 24 };
const ID: SessionId = SessionId(42);
const CHILD_ROOT: &str = "MK097_TEST_ROOT";

fn root(name: &str) -> PathBuf {
    let path = std::env::temp_dir().join(format!("mk097-{name}-{}", std::process::id()));
    fs::remove_dir_all(&path).ok();
    fs::create_dir_all(&path).unwrap();
    path
}

fn key() -> Arc<Key> {
    Arc::new(Key::from_slice(&[1; 32]).unwrap())
}

fn encrypted(root: &Path, key: Arc<Key>) -> Arc<Store> {
    privacy::prepare(&root.join("sessions")).unwrap();
    Store::with_key(
        root.join("sessions"),
        Limits::default(),
        true,
        true,
        Some(key),
    )
}

fn path(store: &Store) -> PathBuf {
    segments(&store.session_dir(ID)).unwrap().remove(0)
}

/// 고정 v1의 zstd 스트림을 고정 키·salt로 감싼다. v2 고정 fixture 생성과 양방향 검사에만 쓴다.
fn fixture_bytes() -> Vec<u8> {
    let mut header = Header::decode(SEGMENT_V1).unwrap();
    header.encrypted = true;
    let prefix = header.encode();
    let salt = [2; SALT_LEN];
    let mut crypto = Stream::open(&key(), &prefix, &salt).unwrap();
    let mut out = prefix.to_vec();
    out.extend_from_slice(&salt);
    let mut cursor = &SEGMENT_V1[HEADER_LEN..];
    while !cursor.is_empty() {
        let len = u32::from_le_bytes(cursor[..4].try_into().unwrap()) as usize;
        let mut body = cursor[8..8 + len].to_vec();
        crypto.seal(&mut body).unwrap();
        out.extend(segment::chunk(&body));
        cursor = &cursor[8 + len..];
    }
    out
}

#[test]
fn encrypted_fixture_roundtrip() {
    let bytes = fixture_bytes();
    assert_eq!(bytes, include_bytes!("../../tests/fixtures/segment-v2.krs"));
    let seg = segment::decode(&bytes, Some(&key())).unwrap().unwrap();
    let old = segment::decode(SEGMENT_V1, None).unwrap().unwrap();
    assert_eq!(seg.records, old.records);
    assert!(seg.header.encrypted);
    assert!(seg.sealed);
    assert_eq!(seg.valid_len, bytes.len() as u64);
    assert!(segment::decode(&bytes, None).is_err());
    assert!(segment::decode(&bytes, Some(&Key::from_slice(&[3; 32]).unwrap())).is_err());
    // 키가 있어도 v1은 평문 경로로 계속 읽힌다.
    assert_eq!(
        segment::decode(SEGMENT_V1, Some(&key()))
            .unwrap()
            .unwrap()
            .records,
        old.records
    );
}

#[test]
fn encrypted_data_resize_raw_and_cold_restore() {
    let root = root("roundtrip");
    let store = encrypted(&root, key());
    let mut rec = store.recorder(ID, SIZE).unwrap();
    let secret = b"MK097-secret-api-token-sk-test-0123456789";
    let data = [secret.as_slice(), b"\r\n"].concat().repeat(500);
    rec.append(&data);
    let size = WinSize {
        cols: 100,
        rows: 30,
    };
    rec.resize(size);
    rec.append(b"done$ ");
    rec.seal();
    let bytes = fs::read(path(&store)).unwrap();
    assert_eq!(
        bytes
            .windows(secret.len())
            .filter(|window| *window == secret)
            .count(),
        0
    );
    let seg = segment::decode(&bytes, Some(&key())).unwrap().unwrap();
    assert_eq!(seg.records[0], Record::Data(data.clone()));
    assert_eq!(seg.records[1], Record::Resize(size));
    assert_eq!(store.written(), bytes.len() as u64);
    let mut raw = Vec::new();
    store
        .raw(ID, 0, data.len() as u64 + 6, |piece| {
            match piece {
                Piece::Data { bytes, .. } => raw.extend(bytes),
                Piece::Gap { .. } => panic!("unexpected gap"),
            }
            Ok(())
        })
        .unwrap();
    assert_eq!(raw, [data.as_slice(), b"done$ "].concat());
    let hist = store.history(ID).unwrap();
    assert!(!hist.page(0).unwrap().rows.is_empty());
    let restored = hist.restore().unwrap();
    assert!(restored.screen_text().contains("done$"));
    assert_eq!(restored.screen().cols, size.cols);
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn encrypted_reopen_with_lost_or_wrong_key_preserves_files() {
    let root = root("wrong-key");
    let store = encrypted(&root, key());
    let mut rec = store.recorder(ID, SIZE).unwrap();
    rec.append(b"secret");
    rec.seal();
    let path = path(&store);
    let before = fs::read(&path).unwrap();
    for key in [None, Some(Arc::new(Key::from_slice(&[7; 32]).unwrap()))] {
        let bad = Store::with_key(root.join("sessions"), Limits::default(), true, true, key);
        assert!(bad.recorder(ID, SIZE).is_err());
        assert!(
            root.join(krill_proto::history::RECORDING_WARNING_FILE)
                .exists()
        );
        assert!(bad.history(ID).unwrap().restore().is_err());
        assert_eq!(fs::read(&path).unwrap(), before);
    }
    assert!(segment::has_encrypted(&root.join("sessions")).unwrap());
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn encrypted_torn_tail_recovers_and_restarts_with_new_salt() {
    let root = root("torn");
    let store = encrypted(&root, key());
    let mut rec = store.recorder(ID, SIZE).unwrap();
    for _ in 0..3 {
        rec.append(b"secret\r\n");
    }
    drop(rec);
    let path = path(&store);
    let bytes = fs::read(&path).unwrap();
    fs::write(&path, &bytes[..bytes.len() - 3]).unwrap();
    let seg = segment::read(&path, Some(&key())).unwrap().unwrap();
    assert_eq!(seg.records.len(), 2);
    assert!(!seg.sealed);
    let mut rec = store.recorder(ID, SIZE).unwrap();
    assert_eq!(fs::metadata(&path).unwrap().len(), seg.valid_len);
    rec.append(b"after\r\n");
    rec.seal();
    let paths = segments(&store.session_dir(ID)).unwrap();
    assert_eq!(paths.len(), 2);
    let next_bytes = fs::read(&paths[1]).unwrap();
    assert_ne!(
        &bytes[HEADER_LEN..HEADER_LEN + SALT_LEN],
        &next_bytes[HEADER_LEN..HEADER_LEN + SALT_LEN]
    );
    let next = segment::decode(&next_bytes, Some(&key())).unwrap().unwrap();
    assert_eq!(next.records[0], Record::Gap(0));
    assert_eq!(next.header.start_seq, seg.end_seq());
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn encrypted_rejects_forged_header_ciphertext_and_reordered_chunks() {
    let bytes = fixture_bytes();
    let mut header = Header::decode(&bytes).unwrap();
    header.session = SessionId(99);
    let mut forged = bytes.clone();
    forged[..HEADER_LEN].copy_from_slice(&header.encode());
    assert!(segment::decode(&forged, Some(&key())).is_err());
    let start = HEADER_LEN + SALT_LEN;
    let len = u32::from_le_bytes(bytes[start..start + 4].try_into().unwrap()) as usize;
    let mut body = bytes[start + 8..start + 8 + len].to_vec();
    body[0] ^= 1;
    let mut forged = bytes.clone();
    forged[start..start + 8 + len].copy_from_slice(&segment::chunk(&body)); // CRC를 다시 계산해도 AEAD가 거부한다.
    assert!(segment::decode(&forged, Some(&key())).is_err());
    let mut duplicated = bytes[..start + 8 + len].to_vec();
    duplicated.extend_from_slice(&bytes[start..]);
    assert!(segment::decode(&duplicated, Some(&key())).is_err());
}

#[test]
fn mixed_v1_v2_append_and_privacy_clear() {
    let root = root("mixed");
    let store = encrypted(&root, key());
    mkdir(&store.session_dir(ID)).unwrap();
    let old = store.session_dir(ID).join("seg-00000000000000000000.krs");
    fs::write(&old, SEGMENT_V1).unwrap();
    let mut rec = store.recorder(ID, SIZE).unwrap();
    assert_eq!(rec.seq(), 12);
    rec.append(b"new secret\r\n");
    rec.seal();
    assert_eq!(fs::read(&old).unwrap(), SEGMENT_V1);
    let hist = store.history(ID).unwrap();
    assert_eq!(hist.len(), 2);
    assert!(hist.restore().unwrap().screen_text().contains("new secret"));
    store.clear().unwrap();
    rec.append(b"after clear\r\n");
    rec.seal();
    let bytes = fs::read(path(&store)).unwrap();
    assert!(Header::decode(&bytes).unwrap().encrypted);
    fs::remove_dir_all(root).unwrap();
}

proptest::proptest! {
    #[test]
    fn arbitrary_segment_bytes_never_panic(bytes in proptest::collection::vec(proptest::prelude::any::<u8>(), 0..1024)) {
        drop(segment::decode(&bytes, Some(&key())));
    }

    #[test]
    fn every_encrypted_truncation_recovers_a_prefix(cut in 0..256_usize) {
        let bytes = fixture_bytes();
        let cut = cut.min(bytes.len());
        if let Some(seg) = segment::decode(&bytes[..cut], Some(&key())).unwrap() {
            proptest::prop_assert!(seg.valid_len <= cut as u64);
            proptest::prop_assert!(seg.records.len() <= 3);
        }
    }
}

#[cfg(target_os = "linux")]
#[test]
fn key_file_is_private_reused_and_required_when_data_exists() {
    use std::os::unix::fs::PermissionsExt as _;
    let root = root("keyfile");
    assert!(
        Key::load(&root, true).is_err(),
        "데이터가 있는데 키가 없으면 새로 만들지 않는다"
    );
    let first = Key::load(&root, false).unwrap();
    let mode = fs::metadata(root.join("segment.key"))
        .unwrap()
        .permissions()
        .mode();
    assert_eq!(mode & 0o777, 0o600);
    let second = Key::load(&root, true).unwrap();
    let store = encrypted(&root, first);
    let mut rec = store.recorder(ID, SIZE).unwrap();
    rec.append(b"secret from key file\r\n");
    rec.seal();
    let bytes = fs::read(path(&store)).unwrap();
    assert!(
        segment::decode(&bytes, Some(&second))
            .unwrap()
            .unwrap()
            .sealed
    );
    fs::remove_dir_all(root).unwrap();
}

#[cfg(target_os = "macos")]
#[test]
fn keychain_roundtrip_in_isolated_child() {
    let root = root("keychain");
    let status = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "store::encryption_tests::keychain_child",
            "--ignored",
            "--nocapture",
        ])
        .env(CHILD_ROOT, &root)
        .env("HOME", &root)
        .env("CFFIXED_USER_HOME", &root)
        .status()
        .unwrap();
    assert!(status.success());
    fs::remove_dir_all(root).unwrap();
}

#[cfg(target_os = "macos")]
#[test]
#[ignore = "keychain_roundtrip_in_isolated_child가 HOME 오버레이로 실행한다"]
fn keychain_child() {
    let Some(root) = std::env::var_os(CHILD_ROOT).map(PathBuf::from) else {
        return;
    };
    let _ui = SecKeychain::disable_user_interaction().unwrap();
    let keychain_path = root.join("test.keychain");
    let keychain = CreateOptions::new()
        .password("mk097-temporary-test-only")
        .create(&keychain_path)
        .unwrap();
    let first = Key::load_from(&root, false, &keychain).unwrap();
    let second = Key::load_from(&root, true, &keychain).unwrap();
    let store = encrypted(&root, first);
    let mut rec = store.recorder(ID, SIZE).unwrap();
    rec.append(b"secret from keychain\r\n");
    rec.seal();
    let bytes = fs::read(path(&store)).unwrap();
    assert!(
        segment::decode(&bytes, Some(&second))
            .unwrap()
            .unwrap()
            .sealed
    );
    // 키가 사라졌으면 새 키를 만들지 않고 오류로 끝난다.
    let lost_path = root.join("lost.keychain");
    let lost = CreateOptions::new()
        .password("mk097-temporary-test-only")
        .create(&lost_path)
        .unwrap();
    assert!(Key::load_from(&root, true, &lost).is_err());
    assert!(
        lost.find_generic_password("com.minikrill.segment-key.v2", "nonexistent")
            .is_err()
    );
    drop((keychain, lost));
    assert!(
        Command::new("/usr/bin/security")
            .arg("delete-keychain")
            .arg(&keychain_path)
            .arg(&lost_path)
            .status()
            .unwrap()
            .success()
    );
}

/// 옵트인 비용 비교: 같은 Store/Recorder, zstd-3, 64MiB 원본, 16KiB append, 토큰 버킷·GC 상한만 해제.
#[test]
#[ignore = "release 성능 실측: --release --run-ignored only encryption_measure --no-capture"]
fn encryption_measure() {
    let root = root("bench");
    let key = key();
    let mut terminal = Vec::new();
    for n in 0..4096 {
        use std::io::Write as _;
        write!(
            terminal,
            "\x1b[32mline-{n:06}\x1b[0m 한글 secret build output {}\r\n",
            n * 17
        )
        .unwrap();
    }
    let corpora = [("terminal", terminal), ("noise", noise(4 << 20, 29))];
    for (name, corpus) in corpora {
        let mut samples = [Vec::new(), Vec::new()];
        let mut reads = [Vec::new(), Vec::new()];
        for run in 0..14 {
            for mode in [run % 2, 1 - run % 2] {
                let dir = root.join(format!("{name}-{run}-{mode}"));
                privacy::prepare(&dir).unwrap();
                let limits = Limits {
                    burst: u64::MAX / 2,
                    rate_per_min: 0,
                    session_bytes: u64::MAX,
                    total_bytes: u64::MAX,
                    ..Limits::default()
                };
                let store =
                    Store::with_key(dir.clone(), limits, true, mode == 1, Some(key.clone()));
                let mut rec = store.recorder(ID, SIZE).unwrap();
                let block = 16 << 10;
                let start = Instant::now();
                for n in 0..4096 {
                    let offset = (n * block) % (corpus.len() - block);
                    rec.append(&corpus[offset..offset + block]);
                }
                rec.seal();
                let elapsed = start.elapsed().as_secs_f64();
                let start = Instant::now();
                let mut count = 0;
                store
                    .raw(ID, 0, 64 << 20, |piece| {
                        if let Piece::Data { bytes, .. } = piece {
                            count += bytes.len();
                        }
                        Ok(())
                    })
                    .unwrap();
                let read = start.elapsed().as_secs_f64();
                assert_eq!(count, 64 << 20);
                if run >= 3 {
                    samples[mode].push(elapsed);
                    reads[mode].push(read);
                }
                drop(rec);
                fs::remove_dir_all(dir).unwrap();
            }
        }
        let median = |v: &mut Vec<f64>| {
            v.sort_by(f64::total_cmp);
            v[v.len() / 2]
        };
        let a = median(&mut samples[0]);
        let b = median(&mut samples[1]);
        let ar = median(&mut reads[0]);
        let br = median(&mut reads[1]);
        eprintln!(
            "MK097 {name} append plain={a:.6}s encrypted={b:.6}s overhead={:.3}% read plain={ar:.6}s encrypted={br:.6}s overhead={:.3}%",
            (b / a - 1.) * 100.,
            (br / ar - 1.) * 100.
        );
    }
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn encrypted_reconstructs_after_process_sigkill() {
    let root = root("kill9");
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "store::encryption_tests::encrypted_writer_child",
            "--ignored",
            "--nocapture",
        ])
        .env(CHILD_ROOT, &root)
        .spawn()
        .unwrap();
    let end = Instant::now() + Duration::from_secs(30);
    while !root.join("ready").exists() {
        assert!(Instant::now() < end, "writer did not become ready");
        std::thread::sleep(Duration::from_millis(5));
    }
    child.kill().unwrap();
    child.wait().unwrap();
    let store = encrypted(&root, key());
    let data = noise(2 << 20, 71);
    let mut raw = Vec::new();
    store
        .raw(ID, 0, data.len() as u64, |piece| {
            if let Piece::Data { bytes, .. } = piece {
                raw.extend(bytes);
            }
            Ok(())
        })
        .unwrap();
    assert_eq!(raw, data);
    let mut rec = store.recorder(ID, SIZE).unwrap();
    rec.append(b"after restart\r\n");
    rec.seal();
    let paths = segments(&store.session_dir(ID)).unwrap();
    let last = segment::read(paths.last().unwrap(), Some(&key()))
        .unwrap()
        .unwrap();
    assert_eq!(last.records[0], Record::Gap(0));
    assert_eq!(last.header.start_seq, data.len() as u64);
    fs::remove_dir_all(root).unwrap();
}

#[test]
#[ignore = "encrypted_reconstructs_after_process_sigkill가 자식으로 실행한다"]
fn encrypted_writer_child() {
    let Some(root) = std::env::var_os(CHILD_ROOT).map(PathBuf::from) else {
        return;
    };
    let store = encrypted(&root, key());
    let mut rec = store.recorder(ID, SIZE).unwrap();
    let data = noise(2 << 20, 71);
    // 마지막 세그먼트는 봉인 없이 죽도록 1MiB 경계와 어긋난 묶음으로 쓴다.
    for block in data.chunks(30_000) {
        rec.append(block);
    }
    fs::write(root.join("ready"), b"ready").unwrap();
    std::thread::sleep(Duration::from_secs(60));
}
