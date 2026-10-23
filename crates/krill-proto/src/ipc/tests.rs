use super::*;

fn server_hello() -> ServerHello {
    ServerHello {
        proto_min: PROTO_MIN,
        proto_max: PROTO_MAX,
        features: 0,
        build: "0.1.0".to_owned(),
        instance_id: 0x0102_0304_0506_0708,
    }
}

fn client_hello_v1() -> ClientHello {
    ClientHello {
        proto_min: 1,
        proto_max: 1,
        features: 0,
        role: ClientRole::Cli,
        name: "mk 한글".to_owned(),
    }
}

fn hex(b: &[u8]) -> String {
    use std::fmt::Write as _;
    b.iter().fold(String::new(), |mut s, x| {
        write!(s, "{x:02x}").unwrap();
        s
    })
}

fn unhex(s: &str) -> Vec<u8> {
    s.as_bytes()
        .chunks(2)
        .map(|p| u8::from_str_radix(std::str::from_utf8(p).unwrap(), 16).unwrap())
        .collect()
}

fn frame(bytes: &[u8]) -> (u8, Vec<u8>) {
    read_frame(&mut &bytes[..]).unwrap().unwrap()
}

/// v1 고정 바이트. 바뀌면 호환 위반이다(새 메시지는 줄을 추가만 한다).
const SERVER_HELLO_V1: &str = "130000000101010005302e312e30888e98a8c0e0808101";
const CLIENT_HELLO_V1: &str = "0f0000000201010001096d6b20ed959ceab880";

#[test]
fn fixtures_v1_both_directions() {
    let s = server_hello().encode().unwrap();
    let c = client_hello_v1().encode().unwrap();
    assert_eq!(hex(&s), SERVER_HELLO_V1);
    assert_eq!(hex(&c), CLIENT_HELLO_V1);

    let (k, body) = frame(&unhex(SERVER_HELLO_V1));
    assert_eq!(ServerHello::decode(k, &body).unwrap(), server_hello());
    let (k, body) = frame(&unhex(CLIENT_HELLO_V1));
    assert_eq!(ClientHello::decode(k, &body).unwrap(), client_hello_v1());
}

/// 현재 krilld는 v1 클라이언트(고정 바이트 그대로)를 받는다. `PROTO_MAX`가 2가 되면 이것이 N-1 검사다.
#[test]
fn current_daemon_accepts_frozen_v1_client() {
    let (k, body) = frame(&unhex(CLIENT_HELLO_V1));
    let c = ClientHello::decode(k, &body).unwrap();
    let n = negotiate(&server_hello(), &c).unwrap();
    assert_eq!(n.proto, 1);
    assert_eq!(n.features, 0);
}

/// `PROTO_MAX`를 올리면서 `PROTO_MIN`을 같이 올려 N-1을 끊는 실수를 막는다.
#[test]
fn proto_min_keeps_n_minus_1() {
    const { assert!(PROTO_MIN <= PROTO_MAX) };
    assert!(PROTO_MIN <= PROTO_MAX.saturating_sub(1).max(1));
}

#[test]
fn negotiate_picks_highest_common_version_and_feature_intersection() {
    let server = ServerHello {
        proto_min: 1,
        proto_max: 2,
        features: 0b0110,
        ..server_hello()
    };
    // N-1 클라이언트
    let old = ClientHello {
        proto_min: 1,
        proto_max: 1,
        features: 0b0010,
        ..client_hello_v1()
    };
    assert_eq!(
        negotiate(&server, &old),
        Some(Negotiated {
            proto: 1,
            features: 0b0010
        })
    );
    // 같은 세대, 데몬이 모르는 feature 비트는 버린다
    let same = ClientHello {
        proto_max: 2,
        features: 0b1111,
        ..old.clone()
    };
    assert_eq!(
        negotiate(&server, &same),
        Some(Negotiated {
            proto: 2,
            features: 0b0110
        })
    );
    // N-2 이하·미래 전용 클라이언트는 겹치지 않는다
    let too_new = ClientHello {
        proto_min: 3,
        proto_max: 4,
        ..old
    };
    assert_eq!(negotiate(&server, &too_new), None);
}

/// 필드를 끝에 붙인 새 DTO도 구 디코더가 읽는다(구 클라이언트 ← 새 krilld, 구 krilld ← 새 클라이언트).
#[test]
fn trailing_fields_are_ignored() {
    #[derive(Serialize)]
    struct ServerHelloNext {
        base: ServerHello,
        boot_id: u64,
    }
    #[derive(Serialize)]
    struct ClientHelloNext {
        base: ClientHello,
        pid: u32,
    }
    let s = ServerHelloNext {
        base: server_hello(),
        boot_id: 9,
    };
    let body = postcard::to_stdvec(&s).unwrap();
    assert_eq!(
        ServerHello::decode(kind::SERVER_HELLO, &body).unwrap(),
        server_hello()
    );

    let c = ClientHelloNext {
        base: ClientHello {
            proto_max: 2,
            features: u64::MAX,
            ..client_hello_v1()
        },
        pid: 7,
    };
    let body = postcard::to_stdvec(&c).unwrap();
    let got = ClientHello::decode(kind::CLIENT_HELLO, &body).unwrap();
    assert_eq!(
        negotiate(&server_hello(), &got),
        Some(Negotiated {
            proto: 1,
            features: server_hello().features
        })
    );
}

#[test]
fn rejects_oversize_frame_without_reading_body() {
    // len = MAX_FRAME + 1, body는 없다. 읽기 전에 거부해야 한다.
    let len = u32::try_from(MAX_FRAME + 1).unwrap();
    let e = read_frame(&mut &len.to_le_bytes()[..]).unwrap_err();
    assert_eq!(e.kind(), io::ErrorKind::InvalidData);
    let e = read_frame(&mut &u32::MAX.to_le_bytes()[..]).unwrap_err();
    assert_eq!(e.kind(), io::ErrorKind::InvalidData);
    let e = read_frame(&mut &0u32.to_le_bytes()[..]).unwrap_err();
    assert_eq!(e.kind(), io::ErrorKind::InvalidData);

    // 정확히 MAX_FRAME은 받는다.
    let max = encode_frame(0x7f, &vec![0xaa; MAX_FRAME - 1]).unwrap();
    let (k, body) = frame(&max);
    assert_eq!((k, body.len()), (0x7f, MAX_FRAME - 1));
    // 쓰는 쪽도 넘는 프레임을 만들지 않는다.
    assert_eq!(
        encode_frame(0x7f, &vec![0; MAX_FRAME]),
        Err(Error::BadLength(u64::try_from(MAX_FRAME).unwrap()))
    );
}

#[test]
fn read_frame_eof_and_truncation() {
    assert!(read_frame(&mut &[][..]).unwrap().is_none());
    let f = client_hello_v1().encode().unwrap();
    let e = read_frame(&mut &f[..f.len() - 1]).unwrap_err();
    assert_eq!(e.kind(), io::ErrorKind::UnexpectedEof);
}

#[test]
fn rejects_wrong_kind_and_long_name() {
    let (k, body) = frame(&server_hello().encode().unwrap());
    assert_eq!(
        ClientHello::decode(k, &body),
        Err(Error::UnexpectedKind {
            want: kind::CLIENT_HELLO,
            got: kind::SERVER_HELLO
        })
    );
    let long = ClientHello {
        name: "a".repeat(NAME_MAX + 1),
        ..client_hello_v1()
    };
    let (k, body) = frame(&long.encode().unwrap());
    assert_eq!(
        ClientHello::decode(k, &body),
        Err(Error::TooLong(NAME_MAX + 1))
    );
    // 모르는 role(미래 variant)은 읽지 않는다.
    let mut body = postcard::to_stdvec(&client_hello_v1()).unwrap();
    let role_at = 1 + 1 + 1; // proto_min·proto_max·features(0)가 각 varint 1바이트
    body[role_at] = 9;
    assert!(matches!(
        ClientHello::decode(kind::CLIENT_HELLO, &body),
        Err(Error::Codec(_))
    ));
}

/// 클라이언트 채널(UDS)의 kind는 모듈이 달라도 한 번호 공간이다(ARCHITECTURE §4.3). 2026-10-15 hook EVENT/ACK가
/// control REQUEST/REPLY와 같은 0x30/0x31로 충돌했던 사고 방지. 새 kind를 만들면 여기에도 추가한다.
#[test]
fn client_channel_kinds_are_unique() {
    let all = client_channel_kinds();
    let mut seen = std::collections::HashMap::new();
    for (name, k) in all {
        assert!(
            seen.insert(k, name).is_none(),
            "duplicate client kind {k:#04x}"
        );
    }
}

#[test]
fn served_usage_is_advertised() {
    assert_ne!(FEATURES & crate::usage::FEAT_USAGE, 0);
}

#[test]
fn feature_registry_matches_architecture_and_frozen_values() {
    let registry = [
        (
            "FEAT_ORCH_RETAIN",
            crate::orchestration::FEAT_ORCH_RETAIN,
            19,
        ),
        ("FEAT_MOBILE", crate::mobile::FEAT_MOBILE, 56),
        ("FEAT_CLIPBOARD", crate::clipboard::FEAT_CLIPBOARD, 33),
        ("FEAT_CONTROL", crate::control::FEAT_CONTROL, 1),
        ("FEAT_WORKTREE", crate::control::FEAT_WORKTREE, 4),
        (
            "FEAT_WORKTREE_REMOVE",
            crate::control::FEAT_WORKTREE_REMOVE,
            5,
        ),
        ("FEAT_GIT_CHANGES", crate::control::FEAT_GIT_CHANGES, 6),
        ("FEAT_CLI", crate::control::FEAT_CLI, 10),
        ("FEAT_WAIT_STATUS", crate::control::FEAT_WAIT_STATUS, 11),
        ("FEAT_SIDEBAR", crate::control::FEAT_SIDEBAR, 14),
        ("FEAT_AGENTS", crate::control::FEAT_AGENTS, 31),
        ("FEAT_SESSION_FORK", crate::control::FEAT_SESSION_FORK, 34),
        ("FEAT_UPGRADE", crate::control::FEAT_UPGRADE, 36),
        ("FEAT_COLD", crate::control::FEAT_COLD, 18),
        ("FEAT_HOLDER_WATCH", crate::daemon::FEAT_HOLDER_WATCH, 7),
        ("FEAT_HISTORY", crate::history::FEAT_HISTORY, 16),
        ("FEAT_HOOK", crate::hook::FEAT_HOOK, 3),
        ("FEAT_INPUT", crate::input::FEAT_INPUT, 2),
        ("FEAT_RESOURCES", crate::resources::FEAT_RESOURCES, 38),
        ("FEAT_ROLES", crate::roles::FEAT_ROLES, 12),
        ("FEAT_SCREEN", crate::screen::FEAT_SCREEN, 0),
        ("FEAT_SCREEN_META", crate::screen::FEAT_SCREEN_META, 9),
        ("FEAT_SEARCH", crate::search::FEAT_SEARCH, 8),
        (
            "FEAT_SEARCH_OPTIONS",
            crate::search::FEAT_SEARCH_OPTIONS,
            20,
        ),
        ("FEAT_TERM_MODES", crate::term_modes::FEAT_TERM_MODES, 13),
        (
            "FEAT_CLEAR_VIEWPORT",
            crate::term_modes::FEAT_CLEAR_VIEWPORT,
            21,
        ),
        ("FEAT_USAGE", crate::usage::FEAT_USAGE, 17),
        (
            "FEAT_PROVIDER_DETAILS",
            crate::usage::FEAT_PROVIDER_DETAILS,
            15,
        ),
        ("FEAT_USAGE_AGY", crate::usage::FEAT_USAGE_AGY, 53),
        ("FEAT_USAGE_RULES", crate::usage::FEAT_USAGE_RULES, 54),
        ("FEAT_HISTORY_PAUSE", crate::history::FEAT_HISTORY_PAUSE, 55),
        ("FEAT_USAGE_STATUS", crate::usage::FEAT_USAGE_STATUS, 57),
        (
            "FEAT_SESSION_ACCOUNT",
            crate::control::FEAT_SESSION_ACCOUNT,
            58,
        ),
        (
            "FEAT_USAGE_RATELIMIT",
            crate::usage::FEAT_USAGE_RATELIMIT,
            60,
        ),
        ("FEAT_USAGE_MANAGE", crate::usage::FEAT_USAGE_MANAGE, 59),
        ("FEAT_ACCOUNT_POLICY", crate::usage::FEAT_ACCOUNT_POLICY, 61),
        ("FEAT_ACCOUNT_CANCEL", crate::usage::FEAT_ACCOUNT_CANCEL, 22),
        (
            "FEAT_ACCOUNT_CONNECTION",
            crate::usage::FEAT_ACCOUNT_CONNECTION,
            62,
        ),
        ("FEAT_THEME", crate::theme::FEAT_THEME, 46),
        ("FEAT_GIT_SYNC", crate::ipc::FEAT_GIT_SYNC, 41),
        ("FEAT_RESUME", crate::control::FEAT_RESUME, 42),
        (
            "FEAT_RESUME_HISTORY",
            crate::control::FEAT_RESUME_HISTORY,
            30,
        ),
        ("FEAT_GIT_WATCH", crate::control::FEAT_GIT_WATCH, 44),
        ("FEAT_HEARTBEAT", crate::daemon::FEAT_HEARTBEAT, 45),
        (
            "FEAT_CLEAR_SCROLLBACK",
            crate::control::FEAT_CLEAR_SCROLLBACK,
            47,
        ),
        ("FEAT_GIT_EDIT", crate::control::FEAT_GIT_EDIT, 48),
        ("FEAT_USAGE_RESET", crate::usage::FEAT_USAGE_RESET, 49),
        (
            "FEAT_AGENT_HISTORY",
            crate::agent_history::FEAT_AGENT_HISTORY,
            50,
        ),
        ("FEAT_AI_VAULT", crate::agent_history::FEAT_AI_VAULT, 32),
        ("FEAT_CHAT_POLL", crate::agent_history::FEAT_CHAT_POLL, 63),
        ("FEAT_HIBERNATION", crate::control::FEAT_HIBERNATION, 51),
        (
            "FEAT_WORKTREE_PREPARE",
            crate::control::FEAT_WORKTREE_PREPARE,
            52,
        ),
    ];
    // 새 상수를 표·fixture 양쪽에서 빠뜨리는 것도 잡는다.
    let declared: std::collections::BTreeSet<_> =
        std::fs::read_dir(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src"))
            .unwrap()
            .map(Result::unwrap)
            .filter(|e| e.path().extension().is_some_and(|ext| ext == "rs"))
            .flat_map(|e| {
                std::fs::read_to_string(e.path())
                    .unwrap()
                    .lines()
                    .filter_map(|line| {
                        line.trim()
                            .strip_prefix("pub const FEAT_")
                            .and_then(|tail| tail.split_once(':'))
                            .map(|(name, _)| format!("FEAT_{name}"))
                    })
                    .collect::<Vec<_>>()
            })
            .collect();
    assert_eq!(
        declared,
        registry
            .iter()
            .map(|(name, _, _)| (*name).to_owned())
            .collect()
    );
    let docs = include_str!("../../../../docs/ARCHITECTURE.md");
    let mut documented = std::collections::BTreeMap::new();
    for line in docs.lines().filter(|line| line.starts_with("| FEAT_")) {
        let mut columns = line.split('|').skip(1).map(str::trim);
        let name = columns.next().unwrap();
        let bit: u32 = columns
            .next()
            .unwrap()
            .trim_matches('`')
            .strip_prefix("1 << ")
            .unwrap()
            .parse()
            .unwrap();
        assert!(documented.insert(name, bit).is_none(), "duplicate {name}");
    }
    assert_eq!(documented.len(), registry.len());
    let mut all = 0;
    for (name, value, bit) in registry {
        assert_eq!(value, 1u64 << bit, "{name} wire value changed");
        assert_eq!(
            documented.remove(name),
            Some(bit),
            "{name} documentation drift"
        );
        assert_eq!(all & value, 0, "{name} overlaps another feature");
        all |= value;
    }
}

fn client_channel_kinds() -> Vec<(&'static str, u8)> {
    use crate::{control, history, hook, input, screen, search};
    vec![
        ("mobile::MOBILE_REQ", crate::mobile::MOBILE_REQ),
        ("mobile::MOBILE_REPLY", crate::mobile::MOBILE_REPLY),
        ("theme::THEME", crate::theme::THEME),
        ("term_modes::TERM_MODES", crate::term_modes::TERM_MODES),
        (
            "term_modes::CLEAR_VIEWPORT",
            crate::term_modes::CLEAR_VIEWPORT,
        ),
        ("ipc::SERVER_HELLO", kind::SERVER_HELLO),
        ("ipc::CLIENT_HELLO", kind::CLIENT_HELLO),
        ("screen::ATTACH", screen::kind::ATTACH),
        ("screen::RESYNC", screen::kind::RESYNC),
        ("screen::HISTORY", screen::kind::HISTORY),
        ("screen::ATTACHED", screen::kind::ATTACHED),
        ("screen::UPDATE", screen::kind::UPDATE),
        ("screen::OUTPUT", screen::kind::OUTPUT),
        ("screen::GAP", screen::kind::GAP),
        ("screen::CLOSED", screen::kind::CLOSED),
        ("screen::REFUSED", screen::kind::REFUSED),
        ("screen::HISTORY_ROWS", screen::kind::HISTORY_ROWS),
        ("search::SEARCH", search::kind::SEARCH),
        ("search::SEARCH_OPTIONS", search::kind::SEARCH_OPTIONS),
        ("search::SEARCH_RESULT", search::kind::SEARCH_RESULT),
        ("input::INPUT", input::kind::INPUT),
        ("input::INPUT_ACK", input::kind::INPUT_ACK),
        ("hook::EVENT", hook::kind::EVENT),
        ("hook::ACK", hook::kind::ACK),
        ("history::HISTORY_REQ", history::kind::HISTORY_REQ),
        ("history::HISTORY_REPLY", history::kind::HISTORY_REPLY),
        ("usage::USAGE_REQ", crate::usage::kind::USAGE_REQ),
        ("usage::USAGE_REPLY", crate::usage::kind::USAGE_REPLY),
        ("control::REQUEST", control::kind::REQUEST),
        ("control::REPLY", control::kind::REPLY),
        ("roles::ROLE", crate::roles::kind::ROLE),
        ("roles::ROLE_ACK", crate::roles::kind::ROLE_ACK),
        ("daemon::HOLDER_STATUS", crate::daemon::HOLDER_STATUS),
        ("clipboard::CLIPBOARD", crate::clipboard::CLIPBOARD),
        ("daemon::PING", crate::daemon::PING),
        ("daemon::PONG", crate::daemon::PONG),
    ]
}

#[test]
fn kind_registry_matches_architecture() {
    let docs = include_str!("../../../../docs/ARCHITECTURE.md");
    let documented: std::collections::BTreeMap<_, _> = docs
        .lines()
        .filter(|line| line.starts_with("| `") && line.contains("::"))
        .map(|line| {
            let mut columns = line.split('|').skip(1).map(str::trim);
            let name = columns.next().unwrap().trim_matches('`');
            let value = columns
                .next()
                .unwrap()
                .trim_matches('`')
                .strip_prefix("0x")
                .unwrap();
            (name, u8::from_str_radix(value, 16).unwrap())
        })
        .collect();
    let actual = client_channel_kinds().into_iter().collect();
    assert_eq!(documented, actual);
}

// PH-05: 프레임 리더와 Hello 디코더는 proptest 대상이다(CONVENTIONS §6).
// `read_frame`은 길이 선언(최대 1MiB)만큼 먼저 할당하므로(감사 확인, 상한 안) 큰 입력도 받는다.
proptest::proptest! {
    #[test]
    fn arbitrary_bytes_never_panic(
        bytes in proptest::collection::vec(proptest::num::u8::ANY, 0..4096),
    ) {
        read_frame(&mut &bytes[..]).ok();
        ServerHello::decode(kind::SERVER_HELLO, &bytes).ok();
        ClientHello::decode(kind::CLIENT_HELLO, &bytes).ok();
    }
}

/// 새 비트는 기존 Hello DTO 안에서만 협상한다. N-1 양방향 fixture는 그대로 읽힌다.
#[test]
fn resume_history_hello_fixtures_and_n_minus_1_negotiation() {
    let mut server = server_hello();
    server.features = crate::control::FEAT_RESUME_HISTORY;
    let mut client = client_hello_v1();
    client.features = crate::control::FEAT_RESUME_HISTORY;
    let server_bytes = "17000000010101808080800405302e312e30888e98a8c0e0808101";
    let client_bytes = "13000000020101808080800401096d6b20ed959ceab880";
    assert_eq!(hex(&server.encode().unwrap()), server_bytes);
    assert_eq!(hex(&client.encode().unwrap()), client_bytes);
    let (kind, body) = frame(&unhex(server_bytes));
    assert_eq!(ServerHello::decode(kind, &body).unwrap(), server);
    let (kind, body) = frame(&unhex(client_bytes));
    assert_eq!(ClientHello::decode(kind, &body).unwrap(), client);
    assert_eq!(
        negotiate(&server, &client).unwrap().features,
        crate::control::FEAT_RESUME_HISTORY
    );
    let (kind, body) = frame(&unhex(CLIENT_HELLO_V1));
    assert_eq!(
        negotiate(&server, &ClientHello::decode(kind, &body).unwrap())
            .unwrap()
            .features,
        0
    );
    let (kind, body) = frame(&unhex(SERVER_HELLO_V1));
    assert_eq!(
        negotiate(&ServerHello::decode(kind, &body).unwrap(), &client)
            .unwrap()
            .features,
        0
    );
}
