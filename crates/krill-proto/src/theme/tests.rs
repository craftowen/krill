use super::*;

/// [`DEFAULT`]의 디스크 바이트. 바뀌면 호환 위반이거나 기본 테마가 바뀐 것이다(그때는 MK-008 근거를 다시 본다).
/// 2026-10-19: 기본 테마를 `Krill Deep`으로 바꿨다(배경 `#10131B`, 커서 `#FF5238`). 배치는 그대로라 호환은 유지된다.
const DEFAULT_V1: &str = "ffffff10131bff52381d1f21cc6666b5bd68f0c67481a2beb294bb8abeb7c5c8c6666666d54e53b9ca4ae7c5477aa6dac397d870c0b1eaeaea";

fn hex(b: &[u8]) -> String {
    use std::fmt::Write as _;
    b.iter().fold(String::new(), |mut s, x| {
        write!(s, "{x:02x}").unwrap();
        s
    })
}

#[test]
fn default_fixture_roundtrip() {
    let bytes = DEFAULT.encode().unwrap();
    assert_eq!(bytes.len(), 57);
    assert_eq!(hex(&bytes), DEFAULT_V1);
    assert_eq!(Theme::decode(&bytes).unwrap(), DEFAULT);
}

/// 기본 배경은 어두워야 한다(OSC 11 응답으로 에이전트가 다크 판정, krilld `is_dark`와 같은 BT.601 가중치).
#[test]
fn default_background_reads_as_dark() {
    let bg = DEFAULT.background;
    assert!(u32::from(bg.r) * 299 + u32::from(bg.g) * 587 + u32::from(bg.b) * 114 < 128_000);
}

#[test]
fn decode_ignores_trailing_fields_and_rejects_short() {
    let mut bytes = DEFAULT.encode().unwrap();
    bytes.extend_from_slice(&[1, 2, 3]);
    assert_eq!(Theme::decode(&bytes).unwrap(), DEFAULT);
    assert!(Theme::decode(&bytes[..56]).is_err());
}

#[test]
fn push_fixture_roundtrip() {
    let value = Push {
        theme: DEFAULT,
        cell: Some(CellSize {
            width: 9,
            height: 18,
        }),
    };
    let frame = Msg::encode(&value).unwrap();
    let mut expected = vec![61, 0, 0, 0, 0x44];
    expected.extend(DEFAULT.encode().unwrap());
    expected.extend([1, 9, 18]);
    assert_eq!(frame, expected);
    let (k, b) = crate::ipc::read_frame(&mut frame.as_slice())
        .unwrap()
        .unwrap();
    assert_eq!(Push::decode(k, &b).unwrap(), value);
}

proptest::proptest! {
    #[test]
    fn arbitrary_push_never_panics(bytes in proptest::collection::vec(proptest::prelude::any::<u8>(), 0..200)) {
        let _decoded = Push::decode(THEME, &bytes);
    }
}
