use super::*;
use crate::ipc::read_frame;

fn hex(b: &[u8]) -> String {
    use std::fmt::Write as _;
    b.iter().fold(String::new(), |mut s, x| {
        write!(s, "{x:02x}").unwrap();
        s
    })
}

fn frame(bytes: &[u8]) -> (u8, Vec<u8>) {
    read_frame(&mut &bytes[..]).unwrap().unwrap()
}

/// `now`에서 깨어 있는 시간 `awake`초, 벽시계 `wall`초 뒤.
fn later(now: Now, awake: u64, wall: u64) -> Now {
    Now {
        awake: now.awake.checked_add(Duration::from_secs(awake)).unwrap(),
        wall: now.wall.checked_add(Duration::from_secs(wall)).unwrap(),
    }
}

fn at(now: Now, secs: u64) -> Now {
    later(now, secs, secs)
}

fn concat(frames: &[Input]) -> Vec<u8> {
    frames.iter().flat_map(|i| i.bytes.clone()).collect()
}

fn ids(frames: &[Input]) -> Vec<u64> {
    frames.iter().map(|f| f.id.0).collect()
}

/// 고정 바이트: 바뀌면 호환 위반이다(모듈 문서 "호환").
#[test]
fn wire_fixtures() {
    let input = Input {
        id: InputId(300),
        bytes: b"ls\r".to_vec(),
    };
    let f = input.encode().unwrap();
    assert_eq!(hex(&f), "0700000013ac02036c730d");
    let (k, b) = frame(&f);
    assert_eq!(Input::decode(k, &b).unwrap(), input);

    for (result, want) in [
        (InputResult::Applied, "0400000027ac0200"),
        (InputResult::Closed, "0400000027ac0201"),
        (InputResult::Denied, "0400000027ac0202"),
    ] {
        let ack = InputAck {
            id: InputId(300),
            result,
        };
        let f = ack.encode().unwrap();
        assert_eq!(hex(&f), want);
        let (k, b) = frame(&f);
        assert_eq!(InputAck::decode(k, &b).unwrap(), ack);
    }
    assert_eq!(kind::INPUT, 0x13);
    assert_eq!(kind::INPUT_ACK, 0x27);
    assert_eq!(FEAT_INPUT, 4);
}

#[test]
fn wrong_kind_is_rejected() {
    let f = InputAck {
        id: InputId(1),
        result: InputResult::Applied,
    }
    .encode()
    .unwrap();
    let (k, b) = frame(&f);
    assert!(Input::decode(k, &b).is_err());
}

#[test]
fn linked_input_is_sent_once_and_acked() {
    let now = Now::get();
    let mut j = Journal::new();
    j.relink(Incarnation::Same, now);
    let f = j.input(b"a", now).unwrap();
    let g = j.input(b"b", now).unwrap();
    assert_eq!(ids(&f), [1]);
    assert_eq!(ids(&g), [2]);
    for id in [1, 2] {
        let ack = InputAck {
            id: InputId(id),
            result: InputResult::Applied,
        };
        assert_eq!(j.ack(&ack), None);
    }
    // 다 ack된 뒤 끊기면 알릴 것이 없다.
    assert_eq!(j.unlink(), None);
}

#[test]
fn big_paste_is_split() {
    let now = Now::get();
    let mut j = Journal::new();
    j.relink(Incarnation::Same, now);
    let paste = vec![b'x'; MAX_INPUT * 2 + 1];
    let f = j.input(&paste, now).unwrap();
    assert_eq!(f.len(), 3);
    assert!(f.iter().all(|i| i.bytes.len() <= MAX_INPUT));
    assert_eq!(concat(&f), paste);
}

#[test]
fn linked_input_budget_rejects_whole_paste_and_ack_restores_capacity() {
    let now = Now::get();
    let mut j = Journal::new();
    j.relink(Incarnation::Same, now);
    let accepted = j.input(&vec![b'x'; IN_FLIGHT_MAX], now).unwrap();
    let last = accepted.last().unwrap().id;
    let rejected = j.input(b"too much", now).unwrap_err();
    assert_eq!(
        rejected,
        Notice {
            fate: Fate::Full,
            bytes: b"too much".to_vec()
        }
    );
    assert_eq!(j.sent.len(), accepted.len());
    assert_eq!(j.last, last.0, "rejection must not consume input IDs");
    j.ack(&InputAck {
        id: last,
        result: InputResult::Applied,
    });
    assert!(j.input(&vec![b'x'; IN_FLIGHT_MAX], now).is_ok());
}

#[test]
fn tiny_inputs_are_bounded_in_both_connection_states() {
    let now = Now::get();
    let mut j = Journal::new();
    j.relink(Incarnation::Same, now);
    for _ in 0..MAX_PENDING_FRAMES {
        j.input(b"x", now).unwrap();
    }
    assert_eq!(j.input(b"y", now).unwrap_err().fate, Fate::Full);
    assert_eq!(j.sent.len(), MAX_PENDING_FRAMES);
    j.unlink();
    for _ in 0..MAX_PENDING_FRAMES {
        j.input(b"x", now).unwrap();
    }
    assert_eq!(j.input(b"y", now).unwrap_err().fate, Fate::Full);
}

/// 보냈지만 ack 전에 끊긴 입력은 다시 보내지 않고 "불확실"로 알린다(중복 실행 0).
/// 끊긴 동안 친 입력은 같은 세션에 다시 붙으면 한 번 보낸다(유실 0).
#[test]
fn unacked_is_uncertain_and_held_is_sent_once() {
    let now = Now::get();
    let mut j = Journal::new();
    j.relink(Incarnation::Same, now);
    j.input(b"one", now).unwrap();
    j.input(b"two", now).unwrap();
    j.ack(&InputAck {
        id: InputId(1),
        result: InputResult::Applied,
    });
    assert_eq!(
        j.unlink(),
        Some(Notice {
            fate: Fate::Uncertain,
            bytes: b"two".to_vec()
        })
    );
    assert!(j.input(b"three", now).unwrap().is_empty());
    assert!(j.input(b"four", now).unwrap().is_empty());
    assert_eq!(j.held_bytes(), 9);

    let (frames, notices) = j.relink(Incarnation::Same, at(now, 1));
    assert!(notices.is_empty());
    assert_eq!(ids(&frames), [3, 4]);
    assert_eq!(concat(&frames), b"threefour");
    assert_eq!(j.held_bytes(), 0);
    // 두 번째 relink는 아무것도 다시 보내지 않는다.
    j.unlink();
    let (again, _) = j.relink(Incarnation::Same, at(now, 2));
    assert!(again.is_empty());
}

#[test]
fn new_incarnation_gets_nothing() {
    let now = Now::get();
    let mut j = Journal::new();
    j.input(b"rm -rf build\r", now).unwrap();
    let (frames, notices) = j.relink(Incarnation::Changed, now);
    assert!(frames.is_empty());
    assert_eq!(
        notices,
        [Notice {
            fate: Fate::NewSession,
            bytes: b"rm -rf build\r".to_vec()
        }]
    );
}

#[test]
fn expired_held_input_is_not_sent() {
    let now = Now::get();
    let mut j = Journal::new();
    j.input(b"old", now).unwrap();
    j.input(b"new", at(now, GRACE.as_secs().checked_sub(1).unwrap()))
        .unwrap();
    let (frames, notices) = j.relink(Incarnation::Same, at(now, GRACE.as_secs()));
    assert_eq!(concat(&frames), b"new");
    assert_eq!(
        notices,
        [Notice {
            fate: Fate::Expired,
            bytes: b"old".to_vec()
        }]
    );
}

/// 유예는 벽시계(잠자기 포함), 탐침은 깨어 있는 시간으로 잰다.
#[test]
fn grace_counts_sleep_probe_does_not() {
    let now = Now::get();
    let mut j = Journal::new();
    j.input(b"before sleep", now).unwrap();
    // 깨어 있는 시간은 1초, 벽시계는 6분(잠자기).
    let woke = later(now, 1, 360);
    let (frames, notices) = j.relink(Incarnation::Same, woke);
    assert!(frames.is_empty());
    assert_eq!(notices.len(), 1);
    assert_eq!(notices[0].fate, Fate::Expired);

    j.input(b"x", woke).unwrap();
    // 한 시간 잤지만 깨어 있던 시간은 1초: 멈춤이 아니다. 깨어 있는 시간으로 PROBE가 지나면 멈춤.
    assert!(!j.stalled(later(woke, 1, 3_600)));
    assert!(j.stalled(later(woke, PROBE.as_secs(), 3_600)));
}

#[test]
fn hold_limit() {
    let now = Now::get();
    let mut j = Journal::new();
    j.input(&vec![b'a'; HOLD_MAX], now).unwrap();
    let err = j.input(b"b", now).unwrap_err();
    assert_eq!(err.fate, Fate::Full);
    assert_eq!(err.bytes, b"b");
    assert_eq!(j.held_bytes(), HOLD_MAX);
}

#[test]
fn closed_session_ack_is_reported() {
    let now = Now::get();
    let mut j = Journal::new();
    j.relink(Incarnation::Same, now);
    j.input(b"a", now).unwrap();
    j.input(b"b", now).unwrap();
    let n = j.ack(&InputAck {
        id: InputId(2),
        result: InputResult::Closed,
    });
    assert_eq!(
        n,
        Some(Notice {
            fate: Fate::SessionClosed,
            bytes: b"b".to_vec()
        })
    );
    // 앞 입력도 처리됐다(순서대로 답한다). 모르는 id는 무시.
    assert_eq!(j.unlink(), None);
    assert_eq!(
        j.ack(&InputAck {
            id: InputId(9),
            result: InputResult::Closed
        }),
        None
    );
}

// PH-05: 입력·ack 디코더는 proptest 대상이다(CONVENTIONS §6).
proptest::proptest! {
    #[test]
    fn arbitrary_bytes_never_panic(
        bytes in proptest::collection::vec(proptest::num::u8::ANY, 0..256),
    ) {
        Input::decode(kind::INPUT, &bytes).ok();
        InputAck::decode(kind::INPUT_ACK, &bytes).ok();
    }
}
