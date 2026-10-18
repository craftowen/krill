use super::*;

fn to_holder() -> Vec<ToHolder> {
    vec![
        ToHolder::Spawn {
            req: 1,
            id: SessionId(7),
            argv: vec![b"/bin/zsh".to_vec(), b"-l".to_vec()],
            env: vec![(b"TERM".to_vec(), b"xterm-256color".to_vec())],
            cwd: "/tmp/한글".as_bytes().to_vec(),
            size: WinSize {
                cols: 120,
                rows: 40,
            },
        },
        ToHolder::Kill {
            req: 2,
            id: SessionId(7),
            signal: 1,
        },
        ToHolder::Release {
            req: 3,
            id: SessionId(7),
        },
        ToHolder::HoldListener { req: 4 },
        ToHolder::UpgradeKrilld {
            req: 5,
            path: b"/k".to_vec(),
        },
        ToHolder::UpgradeHolder {
            req: 6,
            path: b"/h".to_vec(),
        },
    ]
}

fn from_holder() -> Vec<FromHolder> {
    vec![
        FromHolder::Hello {
            version: 1,
            holder_pid: 0x1234,
        },
        FromHolder::Session {
            id: SessionId(7),
            pid: 99,
            status: None,
        },
        FromHolder::Session {
            id: SessionId(8),
            pid: 100,
            status: Some(0x0100),
        },
        FromHolder::Listener,
        FromHolder::Ready,
        FromHolder::Spawned {
            req: 1,
            id: SessionId(7),
            pid: 99,
        },
        FromHolder::Exited {
            id: SessionId(7),
            status: 0x0f00,
        },
        FromHolder::Done { req: 5 },
        FromHolder::Failed { req: 6, errno: 78 },
    ]
}

fn hex(b: &[u8]) -> String {
    use std::fmt::Write as _;
    b.iter().fold(String::new(), |mut s, x| {
        write!(s, "{x:02x}").unwrap();
        s
    })
}

/// v1 고정 바이트. 이 표가 바뀌면 동결 위반이다(새 메시지는 줄을 추가만 한다).
const TO_HOLDER_V1: [&str; 6] = [
    "55000000010001000000070000000000000002000000080000002f62696e2f7a7368020000002d6c01000000040000005445524d0e000000787465726d2d323536636f6c6f720b0000002f746d702fed959ceab88078002800",
    "12000000020002000000070000000000000001000000",
    "0e0000000300030000000700000000000000",
    "06000000040104000000",
    "0c000000050005000000020000002f6b",
    "0c000000060006000000020000002f68",
];
const FROM_HOLDER_V1: [&str; 9] = [
    "080000008100010034120000",
    "0f000000820107000000000000006300000000",
    "1300000082010800000000000000640000000100010000",
    "020000008301",
    "020000008400",
    "12000000850101000000070000000000000063000000",
    "0e00000086000700000000000000000f0000",
    "06000000870005000000",
    "0a0000008800060000004e000000",
];

fn one(b: &[u8]) -> Frame<'_> {
    let (f, n) = parse_frame(b).unwrap().unwrap();
    assert_eq!(n, b.len());
    f
}

#[test]
fn fixtures_v1() {
    for (m, want) in to_holder().iter().zip(TO_HOLDER_V1) {
        let b = m.encode().unwrap();
        assert_eq!(hex(&b), want, "{m:?}");
        assert_eq!(&ToHolder::decode(one(&b)).unwrap(), m);
    }
    for (m, want) in from_holder().iter().zip(FROM_HOLDER_V1) {
        let b = m.encode().unwrap();
        assert_eq!(hex(&b), want, "{m:?}");
        assert_eq!(&FromHolder::decode(one(&b)).unwrap(), m);
    }
}
/// v2에서 추가한 메시지(OWE-478)의 고정 바이트. v1 표는 그대로 두고 여기에 추가만 한다.
#[test]
fn fixtures_v2() {
    let adopt = ToHolder::Adopt {
        req: 8,
        id: SessionId(7),
        pid: 99,
    };
    let b = adopt.encode().unwrap();
    assert_eq!(hex(&b), "12000000070108000000070000000000000063000000");
    assert_eq!(ToHolder::decode(one(&b)), Ok(adopt));
    let mut b0 = b.clone();
    b0[5] = 0;
    assert_eq!(
        ToHolder::decode(one(&b0)),
        Err(Error::FdCount { kind: 0x07, got: 0 }),
        "Adopt는 master fd 1개를 싣는다"
    );
    for (m, want) in [
        (
            FromHolder::Hello {
                version: 2,
                holder_pid: 0x1234,
            },
            "080000008100020034120000",
        ),
        (
            FromHolder::Exited {
                id: SessionId(7),
                status: STATUS_UNKNOWN,
            },
            "0e00000086000700000000000000ffffffff",
        ),
    ] {
        let b = m.encode().unwrap();
        assert_eq!(hex(&b), want, "{m:?}");
        assert_eq!(FromHolder::decode(one(&b)), Ok(m));
    }
}

/// v3에서 추가한 메시지(OWE-416)의 고정 바이트.
#[test]
fn fixtures_v3() {
    let m = ToHolder::Terminate {
        req: 9,
        id: SessionId(7),
    };
    let b = m.encode().unwrap();
    assert_eq!(hex(&b), "0e0000000800090000000700000000000000");
    assert_eq!(ToHolder::decode(one(&b)), Ok(m));
    let hello = FromHolder::Hello {
        version: 3,
        holder_pid: 0x1234,
    };
    assert_eq!(hex(&hello.encode().unwrap()), "080000008100030034120000");
}

/// v4에서 추가한 메시지(OWE-831)의 고정 바이트.
#[test]
fn fixtures_v4() {
    let m = ToHolder::Resize {
        req: 10,
        id: SessionId(7),
        size: WinSize {
            cols: 120,
            rows: 40,
        },
    };
    let b = m.encode().unwrap();
    assert_eq!(hex(&b), "1200000009000a000000070000000000000078002800");
    assert_eq!(ToHolder::decode(one(&b)), Ok(m.clone()));
    // v3 holder에게는 모르는 kind다: fd 없이 오고 req를 읽어 ENOSYS로 답할 수 있다.
    let f = one(&b);
    assert_eq!((f.kind, f.fds), (0x09, 0));
    assert_eq!(request_id(f.body), Some(10));
    // 핸들 없는 kind는 Windows 프레임도 Unix와 바이트가 같다.
    assert_eq!(m.encode_handles(&[]).unwrap(), b);
    let hello = FromHolder::Hello {
        version: VERSION,
        holder_pid: 0x1234,
    };
    assert_eq!(hex(&hello.encode().unwrap()), "080000008100040034120000");
}

/// Windows 핸들 값 프레임: `[len][kind][n][n × u64][body]`. Unix 인코딩은 그대로다(위 v1~v3 표).
#[test]
fn windows_handles() {
    let spawned = FromHolder::Spawned {
        req: 1,
        id: SessionId(7),
        pid: 99,
    };
    let b = spawned.encode_handles(&[0x10, 0x14, 0x18]).unwrap();
    assert_eq!(
        hex(&b),
        "2a0000008503".to_owned()
            + "1000000000000000"
            + "1400000000000000"
            + "1800000000000000"
            + "01000000070000000000000063000000"
    );
    assert_eq!(
        FromHolder::decode_handles(one(&b)),
        Ok((spawned.clone(), vec![0x10, 0x14, 0x18]))
    );
    // 선택 핸들(ref·proc)까지 5개.
    let session = FromHolder::Session {
        id: SessionId(8),
        pid: 100,
        status: None,
    };
    let h = [1, 2, 3, 4, 0xffff_ffff_ffff_fffe];
    let b = session.encode_handles(&h).unwrap();
    assert_eq!(
        FromHolder::decode_handles(one(&b)),
        Ok((session, h.to_vec()))
    );
    let adopt = ToHolder::Adopt {
        req: 2,
        id: SessionId(8),
        pid: 100,
    };
    let b = adopt.encode_handles(&h[..3]).unwrap();
    assert_eq!(
        ToHolder::decode_handles(one(&b)),
        Ok((adopt, h[..3].to_vec()))
    );
    for (m, n) in [
        (ToHolder::HoldListener { req: 3 }.encode_handles(&[9]), 1),
        (FromHolder::Listener.encode_handles(&[9]), 1),
    ] {
        assert_eq!(one(&m.unwrap()).fds, n);
    }
    // 핸들 수가 kind와 맞지 않으면 보내지도 받지도 않는다.
    assert_eq!(
        spawned.encode_handles(&[1, 2]),
        Err(Error::FdCount { kind: 0x85, got: 2 }),
        "sig가 빠진 세션"
    );
    assert_eq!(
        FromHolder::Done { req: 1 }.encode_handles(&[1]),
        Err(Error::FdCount { kind: 0x87, got: 1 })
    );
    assert_eq!(
        FromHolder::Listener.encode_handles(&[]),
        Err(Error::FdCount { kind: 0x83, got: 0 })
    );
    let mut b = FromHolder::Done { req: 1 }.encode().unwrap();
    b[5] = 1;
    assert_eq!(FromHolder::decode_handles(one(&b)), Err(Error::Truncated));
}

#[test]
fn windows_unknown_kind_handles_can_be_closed() {
    // 미래 kind 0x40이 핸들 2개를 싣고 오면 kind를 몰라도 값을 꺼내 닫고 req로 답한다.
    let mut b = vec![0x16, 0, 0, 0, 0x40, 2];
    b.extend(5u64.to_le_bytes());
    b.extend(6u64.to_le_bytes());
    b.extend(42u32.to_le_bytes());
    let f = one(&b);
    assert_eq!(ToHolder::decode_handles(f), Err(Error::UnknownKind(0x40)));
    let (h, body) = f.handles().unwrap();
    assert_eq!(h, vec![5, 6]);
    assert_eq!(request_id(body), Some(42));
}

#[test]
fn partial_buffer_waits() {
    let b = to_holder()[0].encode().unwrap();
    for cut in 0..b.len() {
        assert_eq!(parse_frame(&b[..cut]), Ok(None), "cut {cut}");
    }
    // 두 프레임이 붙어 와도 첫 프레임 길이만 소비한다.
    let mut two = b.clone();
    two.extend(FromHolder::Ready.encode().unwrap());
    let (_, n) = parse_frame(&two).unwrap().unwrap();
    assert_eq!(n, b.len());
}

#[test]
fn trailing_fields_are_ignored() {
    // N+1 송신자가 body 끝에 필드를 붙여도 v1 수신자는 읽는다.
    let mut b = FromHolder::Done { req: 9 }.encode().unwrap();
    b.extend([0xaa, 0xbb]);
    b[0] += 2;
    assert_eq!(FromHolder::decode(one(&b)), Ok(FromHolder::Done { req: 9 }));
    let mut b = ToHolder::Release {
        req: 3,
        id: SessionId(1),
    }
    .encode()
    .unwrap();
    b.push(0xcc);
    b[0] += 1;
    assert_eq!(
        ToHolder::decode(one(&b)),
        Ok(ToHolder::Release {
            req: 3,
            id: SessionId(1)
        })
    );
}

#[test]
fn unknown_kind_keeps_req() {
    // 미래 요청 kind 0x40: holder는 모르는 kind로 거절하고 req로 답할 수 있어야 한다.
    let b = [7, 0, 0, 0, 0x40, 2, 0x2a, 0, 0, 0, 0xff];
    let f = one(&b);
    assert_eq!(f.fds, 2);
    assert_eq!(ToHolder::decode(f), Err(Error::UnknownKind(0x40)));
    assert_eq!(request_id(f.body), Some(42));
    assert_eq!(FromHolder::decode(f), Err(Error::UnknownKind(0x40)));
}

#[test]
fn rejects_bad_frames() {
    assert_eq!(parse_frame(&[1, 0, 0, 0, 0x87]), Err(Error::BadLength(1)));
    let big = u32::try_from(MAX_FRAME + 1).unwrap().to_le_bytes();
    assert_eq!(
        parse_frame(&big),
        Err(Error::BadLength(u32::from_le_bytes(big)))
    );
    // fd 수가 kind와 다르면 거절(fd 대기열이 어긋난 것).
    let mut b = FromHolder::Listener.encode().unwrap();
    b[5] = 0;
    assert_eq!(
        FromHolder::decode(one(&b)),
        Err(Error::FdCount { kind: 0x83, got: 0 })
    );
    // body가 짧으면 Truncated.
    let b = [6, 0, 0, 0, 0x87, 0, 1, 0, 0, 0][..8].to_vec();
    let mut b = b;
    b[0] = 4;
    assert_eq!(FromHolder::decode(one(&b)), Err(Error::Truncated));
    // Option 태그 오류.
    let mut b = from_holder()[1].encode().unwrap();
    *b.last_mut().unwrap() = 2;
    assert_eq!(FromHolder::decode(one(&b)), Err(Error::BadTag(2)));
    // 큰 Spawn은 인코딩 단계에서 거절.
    let huge = ToHolder::UpgradeHolder {
        req: 1,
        path: vec![b'a'; MAX_FRAME],
    };
    assert!(matches!(huge.encode(), Err(Error::BadLength(_))));
}

#[test]
fn random_bytes_never_panic() {
    // ponytail: xorshift 무작위 입력. 커버리지 기반 퍼징은 MK-056.
    let mut x: u64 = 0x9e37_79b9_7f4a_7c15;
    let mut seeds: Vec<Vec<u8>> = to_holder().iter().map(|m| m.encode().unwrap()).collect();
    seeds.extend(from_holder().iter().map(|m| m.encode().unwrap()));
    for i in 0..200_000 {
        let mut b = seeds[i % seeds.len()].clone();
        for _ in 0..=i % 4 {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            let at = usize::try_from(x % 1024).unwrap() % b.len();
            b[at] = x.to_le_bytes()[4];
        }
        if let Ok(Some((f, _))) = parse_frame(&b) {
            ToHolder::decode(f).ok();
            FromHolder::decode(f).ok();
            ToHolder::decode_handles(f).ok();
            FromHolder::decode_handles(f).ok();
        }
    }
}

// PH-05: holder 프레임·양방향 디코더는 proptest 대상이다(CONVENTIONS §6).
proptest::proptest! {
    #[test]
    fn arbitrary_bytes_never_panic(
        bytes in proptest::collection::vec(proptest::num::u8::ANY, 0..256),
    ) {
        if let Ok(Some((f, _))) = parse_frame(&bytes) {
            ToHolder::decode(f).ok();
            FromHolder::decode(f).ok();
            ToHolder::decode_handles(f).ok();
            FromHolder::decode_handles(f).ok();
        }
    }
}
