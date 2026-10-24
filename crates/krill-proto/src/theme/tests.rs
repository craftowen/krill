use super::*;

/// 이전 심해 기본값의 고정 바이트. 해구로 기본값을 바꿔도 기존 저장 파일을 읽을 수 있어야 한다.
const DEEP_V1: &str = "ffffff10131bff52381d1f21cc6666b5bd68f0c67481a2beb294bb8abeb7c5c8c6666666d54e53b9ca4ae7c5477aa6dac397d870c0b1eaeaea";

/// 2026-10-24: 기본값을 해구로 변경. 필드 배치와 57바이트 길이는 그대로다.
const DEFAULT_V1: &str = "dfedf2061620d3e5ee6f8b9ce57a8b8fc7acd4c77d82b6e1b49fd783c3cec8dce694aebcf79aa8a9e1c3eddfa0a4d0f3cdb9eda3dce4edf6fa";

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

#[test]
fn previous_deep_fixture_still_roundtrips() {
    let bytes: Vec<u8> = DEEP_V1
        .as_bytes()
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16).unwrap())
        .collect();
    let theme = Theme::decode(&bytes).unwrap();
    assert_eq!(theme.foreground, rgb(0xff, 0xff, 0xff));
    assert_eq!(theme.background, rgb(0x10, 0x13, 0x1b));
    assert_eq!(theme.cursor, rgb(0xff, 0x52, 0x38));
    assert_eq!(theme.encode().unwrap(), bytes);
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
