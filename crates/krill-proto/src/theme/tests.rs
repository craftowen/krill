use super::*;

/// [`DEFAULT`]의 디스크 바이트. 바뀌면 호환 위반이거나 기본 테마가 바뀐 것이다(그때는 MK-008 근거를 다시 본다).
const DEFAULT_V1: &str = "d0d0d01c1c1cd0d0d0000000cd31310dbc79e5e5102472c8bc3fbc11a8cde5e5e5666666f14c4c23d18bf5f5433b8eead670d629b8dbffffff";

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
