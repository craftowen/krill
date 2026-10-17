use std::os::unix::fs::PermissionsExt as _;

use super::*;

const OVER_PAYLOAD: usize = MAX_PAYLOAD.checked_add(1).unwrap();
const OVER_NAME: usize = NAME_MAX.checked_add(1).unwrap();

fn event() -> HookEvent {
    HookEvent {
        id: EventId {
            pid: 4242,
            unix_ns: 1_760_000_000_123_456_789,
        },
        agent: "claude".to_owned(),
        ppid: 77,
        input: Input::Complete,
        payload: br#"{"hook_event_name":"Stop"}"#.to_vec(),
    }
}

fn hex(b: &[u8]) -> String {
    use std::fmt::Write as _;
    b.iter().fold(String::new(), |mut s, x| {
        write!(s, "{x:02x}").unwrap();
        s
    })
}

fn temp(name: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("mk038-proto-{}-{name}", std::process::id()));
    std::fs::remove_dir_all(&d).ok();
    d
}

fn far() -> Instant {
    Instant::now().checked_add(Duration::from_secs(5)).unwrap()
}

/// v1 고정 바이트. 바뀌면 호환 위반이다(spool에 남은 구 레코드도 이 형식이다).
const EVENT_V1: &str = "30000000149221959aafe0cdd5b1b61806636c617564654d001a7b22686f6f6b5f6576656e745f6e616d65223a2253746f70227d";

#[test]
fn fixture_v1_roundtrip() {
    let f = event().encode().unwrap();
    assert_eq!(hex(&f), EVENT_V1);
    let (k, body) = ipc::read_frame(&mut &f[..]).unwrap().unwrap();
    assert_eq!(HookEvent::decode(k, &body).unwrap(), event());
    assert_eq!(ack(), ipc::encode_frame(kind::ACK, &[]).unwrap());
}

#[test]
fn limits_both_directions() {
    let max = HookEvent {
        payload: vec![b'x'; MAX_PAYLOAD],
        agent: "a".repeat(NAME_MAX),
        ..event()
    };
    let f = max.encode().unwrap();
    assert!(f.len() <= ipc::MAX_FRAME, "max event must fit one frame");
    assert_eq!(read_spool(&f), vec![max.clone()]);

    let big = HookEvent {
        payload: vec![0; OVER_PAYLOAD],
        ..max.clone()
    };
    assert_eq!(big.encode(), Err(Error::TooLong(OVER_PAYLOAD)));
    // 쓰는 쪽을 우회한 과대 레코드도 받는 쪽이 거부한다.
    let raw = ipc::encode(kind::EVENT, &big).unwrap();
    let (k, body) = ipc::read_frame(&mut &raw[..]).unwrap().unwrap();
    assert_eq!(
        HookEvent::decode(k, &body),
        Err(Error::TooLong(OVER_PAYLOAD))
    );
    let long_name = HookEvent {
        agent: "a".repeat(OVER_NAME),
        ..event()
    };
    assert_eq!(long_name.encode(), Err(Error::TooLong(OVER_NAME)));
}

/// 크래시로 잘린 꼬리: 모든 자르는 위치에서 완전한 레코드만 나오고 패닉하지 않는다.
#[test]
fn read_spool_stops_at_truncated_tail() {
    let a = event();
    let b = HookEvent {
        input: Input::TimedOut,
        payload: Vec::new(),
        ..event()
    };
    let fa = a.encode().unwrap();
    let mut bytes = fa.clone();
    bytes.extend(b.encode().unwrap());
    for cut in 0..=bytes.len() {
        let got = read_spool(&bytes[..cut]);
        let want = match cut {
            c if c == bytes.len() => vec![a.clone(), b.clone()],
            c if c >= fa.len() => vec![a.clone()],
            _ => Vec::new(),
        };
        assert_eq!(got, want, "cut at {cut}");
    }
}

#[test]
fn read_spool_skips_corrupt_record() {
    let mut bytes = event().encode().unwrap();
    bytes.extend([5, 0, 0, 0, 0x7f, 1, 2, 3, 4]); // 다른 kind
    bytes.extend(event().encode().unwrap());
    assert_eq!(read_spool(&bytes), vec![event(), event()]);
    let mut bytes = event().encode().unwrap();
    bytes.extend(u32::MAX.to_le_bytes()); // 터무니없는 길이
    assert_eq!(read_spool(&bytes), vec![event()]);
}

#[test]
fn append_spool_creates_private_file_and_appends() {
    let dir = temp("append");
    let path = dir.join("state").join(SPOOL_FILE);
    let f = event().encode().unwrap();
    append_spool(&path, &f, far()).unwrap();
    append_spool(&path, &f, far()).unwrap();
    assert_eq!(
        read_spool(&std::fs::read(&path).unwrap()),
        vec![event(), event()]
    );
    let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode(&path), 0o600);
    assert_eq!(mode(path.parent().unwrap()), 0o700);
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn append_spool_refuses_when_full() {
    let dir = temp("full");
    let path = dir.join(SPOOL_FILE);
    std::fs::create_dir_all(&dir).unwrap();
    File::create(&path).unwrap().set_len(SPOOL_MAX).unwrap();
    let e = append_spool(&path, &event().encode().unwrap(), far()).unwrap_err();
    assert_eq!(e.kind(), io::ErrorKind::Other);
    assert_eq!(std::fs::metadata(&path).unwrap().len(), SPOOL_MAX);
    std::fs::remove_dir_all(&dir).ok();
}

/// 수거하는 쪽이 락을 쥐고 있으면 기다렸다가, 마감이 지나면 락 없이라도 붙인다(fail-open, 이벤트를 버리지 않는다).
#[test]
fn append_spool_waits_for_lock_then_gives_up_on_deadline() {
    let dir = temp("lock");
    let path = dir.join(SPOOL_FILE);
    std::fs::create_dir_all(&dir).unwrap();
    let held = File::create(&path).unwrap();
    held.lock().unwrap();
    let start = Instant::now();
    append_spool(
        &path,
        &event().encode().unwrap(),
        start.checked_add(Duration::from_millis(20)).unwrap(),
    )
    .unwrap();
    let waited = start.elapsed();
    assert!(waited >= Duration::from_millis(20), "{waited:?}");
    assert!(waited < Duration::from_secs(1), "{waited:?}");
    assert_eq!(read_spool(&std::fs::read(&path).unwrap()), vec![event()]);
    std::fs::remove_dir_all(&dir).ok();
}

// PH-05: 훅 이벤트 디코더와 스풀 리더는 proptest 대상이다(CONVENTIONS §6).
proptest::proptest! {
    #[test]
    fn arbitrary_bytes_never_panic(
        bytes in proptest::collection::vec(proptest::num::u8::ANY, 0..512),
    ) {
        HookEvent::decode(kind::EVENT, &bytes).ok();
        read_spool(&bytes);
    }
}
