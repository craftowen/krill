//! Wire contract tests: fixed v1 fixtures, round trips, and parser properties over arbitrary bytes.

use proptest::prelude::*;

use super::*;

fn frame(json: &str) -> Inbound {
    serde_json::from_str(json).unwrap()
}

#[test]
fn parses_request_and_controls() {
    let request = frame(
        r#"{"type":"request","epoch":"E","context":"C","seq":3,"deadlineMs":500,
            "op":{"kind":"set_value","target":{"snapshot":"E/C/1","element":"e2"},"text":"hi","verify":true}}"#,
    );
    let Inbound::Request(request) = request else {
        panic!("not a request");
    };
    assert_eq!(request.seq, 3);
    assert_eq!(request.deadline_ms, 500);
    assert!(request.op.is_mutation());
    assert_eq!(
        frame(r#"{"type":"cancel","seq":9}"#),
        Inbound::Cancel { seq: 9 }
    );
    assert_eq!(frame(r#"{"type":"shutdown"}"#), Inbound::Shutdown);
}

#[test]
fn rejects_unknown_ops_negative_seq_and_missing_deadline() {
    for bad in [
        r#"{"type":"request","epoch":"E","context":"C","seq":1,"deadlineMs":5,"op":{"kind":"screenshot"}}"#,
        r#"{"type":"request","epoch":"E","context":"C","seq":-1,"deadlineMs":5,"op":{"kind":"apps"}}"#,
        r#"{"type":"request","epoch":"E","context":"C","seq":1,"op":{"kind":"apps"}}"#,
        r#"{"type":"status"}"#,
        r#"{"type":"nope"}"#,
    ] {
        assert!(serde_json::from_str::<Inbound>(bad).is_err(), "{bad}");
    }
}

#[test]
fn validation_bounds_text_names_and_limits() {
    let target = Target {
        snapshot: "E/C/1".into(),
        element: "e0".into(),
    };
    let long = "가".repeat(MAX_TEXT_UTF16 + 1);
    assert!(
        Op::SetValue {
            target: target.clone(),
            text: long,
            verify: false
        }
        .validate()
        .is_err()
    );
    // An astral char is two UTF-16 units: the bound counts units, not chars.
    let astral = "😀".repeat(MAX_TEXT_UTF16 / 2 + 1);
    assert!(
        Op::TypeText {
            target: target.clone(),
            text: astral
        }
        .validate()
        .is_err()
    );
    assert!(
        Op::TypeText {
            target: target.clone(),
            text: String::new()
        }
        .validate()
        .is_err()
    );
    assert!(
        Op::SetValue {
            target: target.clone(),
            text: String::new(),
            verify: true
        }
        .validate()
        .is_ok()
    );
    assert!(
        Op::Action {
            target,
            name: String::new()
        }
        .validate()
        .is_err()
    );
    assert!(
        Op::Observe {
            pid: 0,
            window: None,
            limit: None
        }
        .validate()
        .is_err()
    );
    assert!(
        Op::Observe {
            pid: 1,
            window: None,
            limit: Some(MAX_OBSERVE_NODES + 1)
        }
        .validate()
        .is_err()
    );
}

#[test]
fn batch_accepts_only_bounded_flat_mutations() {
    let click = || Op::Click {
        target: Target {
            snapshot: "E/C/1".into(),
            element: "e0".into(),
        },
    };
    let batch = |steps: Vec<Op>| Op::Batch { steps };
    assert!(batch(vec![click()]).is_mutation());
    assert!(batch(vec![click(); MAX_BATCH_STEPS]).validate().is_ok());
    for bad in [
        batch(vec![]),
        batch(vec![click(); MAX_BATCH_STEPS + 1]),
        batch(vec![click(), Op::Apps]),
        batch(vec![Op::Observe {
            pid: 1,
            window: None,
            limit: None,
        }]),
        batch(vec![batch(vec![click()])]),
        // Each step keeps its own bounds.
        batch(vec![Op::TypeText {
            target: Target {
                snapshot: "E/C/1".into(),
                element: "e0".into(),
            },
            text: String::new(),
        }]),
    ] {
        assert!(bad.validate().is_err(), "{bad:?}");
    }
}

#[test]
fn state_for_separates_known_partial_from_unknown() {
    let partial = Delivery::Partial {
        issued_units: 1,
        reason: "element_gone".into(),
    };
    let incomplete = Cleanup::Incomplete {
        reason: "key_up_failed".into(),
    };
    assert_eq!(
        Outcome::state_for(&partial, &Cleanup::Completed),
        RequestState::Completed
    );
    assert_eq!(
        Outcome::state_for(&partial, &incomplete),
        RequestState::Unknown
    );
    assert_eq!(
        Outcome::state_for(&Delivery::Submitted { issued_units: 1 }, &incomplete),
        RequestState::Unknown
    );
    assert_eq!(
        Outcome::state_for(
            &Delivery::Unknown {
                reason: "ax_timeout".into()
            },
            &Cleanup::Completed
        ),
        RequestState::Unknown
    );
    assert_eq!(
        Outcome::state_for(
            &Delivery::NotDispatched {
                reason: "cancelled".into()
            },
            &Cleanup::NotNeeded
        ),
        RequestState::NotDispatched
    );
}

#[test]
fn digest_distinguishes_payloads() {
    let a = RequestFrame {
        epoch: "E".into(),
        context: "C".into(),
        seq: 1,
        deadline_ms: 100,
        op: Op::Apps,
    };
    let mut b = a.clone();
    assert_eq!(a.digest(), b.digest());
    b.deadline_ms = 101;
    assert_ne!(a.digest(), b.digest());
}

#[test]
fn frame_reader_enforces_limit_and_handles_eof() {
    let mut buf = Vec::new();
    let mut input: &[u8] = b"{\"a\":1}\r\n{\"b\":2}";
    assert!(read_frame(&mut input, &mut buf).unwrap());
    assert_eq!(buf, b"{\"a\":1}");
    assert!(read_frame(&mut input, &mut buf).unwrap());
    assert_eq!(buf, b"{\"b\":2}");
    assert!(!read_frame(&mut input, &mut buf).unwrap());

    let mut exact = vec![b'x'; MAX_FRAME_BYTES];
    exact.push(b'\n');
    let mut input = exact.as_slice();
    assert!(read_frame(&mut input, &mut buf).unwrap());
    assert_eq!(buf.len(), MAX_FRAME_BYTES);

    let over = vec![b'x'; MAX_FRAME_BYTES + 1];
    let mut input = over.as_slice();
    assert_eq!(
        read_frame(&mut input, &mut buf).unwrap_err().kind(),
        io::ErrorKind::InvalidData
    );
}

#[test]
fn outbound_axes_serialize_independently() {
    let outcome = Outcome {
        seq: 4,
        state: Outcome::state_for(
            &Delivery::Submitted { issued_units: 1 },
            &Cleanup::NotNeeded,
        ),
        delivery: Delivery::Submitted { issued_units: 1 },
        observation: Observation::Failed {
            reason: "ax_timeout".into(),
        },
        verification: Verification::Unknown {
            reason: "readback_failed".into(),
        },
        cleanup: Cleanup::NotNeeded,
        error: None,
        data: None,
    };
    let json = serde_json::to_value(Outbound::Result(Box::new(outcome))).unwrap();
    assert_eq!(json["type"], "result");
    assert_eq!(json["state"], "completed");
    assert_eq!(json["delivery"]["state"], "submitted");
    assert_eq!(json["delivery"]["issuedUnits"], 1);
    assert_eq!(json["observation"]["state"], "failed");
    assert_eq!(json["verification"]["state"], "unknown");
}

// ---- Fixed `krill-computer-worker/1` fixtures. A failing fixture is a contract change, not a test to update. ----

#[test]
fn inbound_v1_fixtures_parse() {
    let target = || Target {
        snapshot: "E/C/1".into(),
        element: "e2".into(),
    };
    let request = |seq, op| {
        Parsed::Inbound(Inbound::Request(RequestFrame {
            epoch: "E".into(),
            context: "C".into(),
            seq,
            deadline_ms: 500,
            op,
        }))
    };
    let fixtures: [(&[u8], Parsed); 13] = [
        (br#"{"type":"request","epoch":"E","context":"C","seq":1,"deadlineMs":500,"op":{"kind":"capabilities"}}"#, request(1, Op::Capabilities)),
        (br#"{"type":"request","epoch":"E","context":"C","seq":2,"deadlineMs":500,"op":{"kind":"permissions"}}"#, request(2, Op::Permissions)),
        (br#"{"type":"request","epoch":"E","context":"C","seq":3,"deadlineMs":500,"op":{"kind":"apps"}}"#, request(3, Op::Apps)),
        (br#"{"type":"request","epoch":"E","context":"C","seq":4,"deadlineMs":500,"op":{"kind":"observe","pid":42}}"#, request(4, Op::Observe { pid: 42, window: None, limit: None })),
        (br#"{"type":"request","epoch":"E","context":"C","seq":5,"deadlineMs":500,"op":{"kind":"observe","pid":42,"window":1,"limit":50}}"#, request(5, Op::Observe { pid: 42, window: Some(1), limit: Some(50) })),
        (br#"{"type":"request","epoch":"E","context":"C","seq":6,"deadlineMs":500,"op":{"kind":"action","target":{"snapshot":"E/C/1","element":"e2"},"name":"AXPress"}}"#, request(6, Op::Action { target: target(), name: "AXPress".into() })),
        (br#"{"type":"request","epoch":"E","context":"C","seq":7,"deadlineMs":500,"op":{"kind":"set_value","target":{"snapshot":"E/C/1","element":"e2"},"text":"hi"}}"#, request(7, Op::SetValue { target: target(), text: "hi".into(), verify: false })),
        (br#"{"type":"request","epoch":"E","context":"C","seq":8,"deadlineMs":500,"op":{"kind":"click","target":{"snapshot":"E/C/1","element":"e2"}}}"#, request(8, Op::Click { target: target() })),
        (br#"{"type":"request","epoch":"E","context":"C","seq":9,"deadlineMs":500,"op":{"kind":"type_text","target":{"snapshot":"E/C/1","element":"e2"},"text":"\ud55c"}}"#, request(9, Op::TypeText { target: target(), text: "한".into() })),
        (br#"{"type":"request","epoch":"E","context":"C","seq":10,"deadlineMs":500,"op":{"kind":"batch","steps":[{"kind":"click","target":{"snapshot":"E/C/1","element":"e2"}},{"kind":"type_text","target":{"snapshot":"E/C/1","element":"e2"},"text":"hi"}]}}"#, request(10, Op::Batch { steps: vec![Op::Click { target: target() }, Op::TypeText { target: target(), text: "hi".into() }] })),
        (br#"{"type":"status","seq":9}"#, Parsed::Inbound(Inbound::Status { seq: 9 })),
        (br#"{"type":"cancel","seq":9}"#, Parsed::Inbound(Inbound::Cancel { seq: 9 })),
        (br#"{"type":"shutdown"}"#, Parsed::Inbound(Inbound::Shutdown)),
    ];
    for (bytes, expected) in fixtures {
        assert_eq!(
            parse_line(bytes).unwrap(),
            expected,
            "{}",
            String::from_utf8_lossy(bytes)
        );
    }
    assert_eq!(parse_line(b"  \t").unwrap(), Parsed::Blank);
    assert!(parse_line(b"{invalid-json}").is_err());
    assert!(matches!(
        parse_line(br#"{"type":"request","seq":4,"op":{"kind":"screenshot"}}"#).unwrap(),
        Parsed::Rejected { seq: Some(4), .. }
    ));
}

#[test]
fn outbound_v1_fixtures_serialize_exactly() {
    let line = |frame: &Outbound| String::from_utf8(encode(frame).unwrap()).unwrap();
    let fixtures = [
        (
            Outbound::Hello {
                contract: CONTRACT,
                epoch: "E".into(),
                context: "C".into(),
                pid: 7,
                supported: true,
                capabilities: vec!["observe"],
                unsupported: vec!["screenshot"],
                limits: Limits::current(),
            },
            r#"{"type":"hello","contract":"krill-computer-worker/1","epoch":"E","context":"C","pid":7,"supported":true,"capabilities":["observe"],"unsupported":["screenshot"],"limits":{"maxFrameBytes":1048576,"maxDeadlineMs":30000,"maxSeqGap":1024,"maxTextUtf16":4096,"maxObserveNodes":2000,"queueCapacity":4,"ledgerCapacity":64,"maxBatchSteps":4}}"#,
        ),
        (
            Outbound::Accepted { seq: 3 },
            r#"{"type":"accepted","seq":3}"#,
        ),
        (
            Outbound::Rejected {
                seq: None,
                code: RejectCode::InvalidRequest,
                message: "m".into(),
            },
            r#"{"type":"rejected","code":"invalid_request","message":"m"}"#,
        ),
        (
            Outbound::Duplicate {
                seq: 3,
                state: RequestState::Dispatching,
            },
            r#"{"type":"duplicate","seq":3,"state":"dispatching"}"#,
        ),
        (
            Outbound::Status {
                seq: 3,
                state: RequestState::Expired,
                summary: None,
            },
            r#"{"type":"status","seq":3,"state":"expired"}"#,
        ),
        (
            Outbound::Status {
                seq: 3,
                state: RequestState::Completed,
                summary: Some(Summary {
                    delivery: "partial",
                    issued_units: Some(2),
                    verification: "not_requested",
                    cleanup: "completed",
                    error_code: Some("batch_stopped".into()),
                }),
            },
            r#"{"type":"status","seq":3,"state":"completed","summary":{"delivery":"partial","issuedUnits":2,"verification":"not_requested","cleanup":"completed","errorCode":"batch_stopped"}}"#,
        ),
        (
            Outbound::Cancel {
                seq: 3,
                outcome: CancelOutcome::TooLate,
            },
            r#"{"type":"cancel","seq":3,"outcome":"too_late"}"#,
        ),
        (
            Outbound::Result(Box::new(Outcome::not_dispatched(3, "lock_timeout", "m"))),
            r#"{"type":"result","seq":3,"state":"not_dispatched","delivery":{"state":"not_dispatched","reason":"lock_timeout"},"observation":{"state":"not_requested"},"verification":{"state":"not_requested"},"cleanup":{"state":"not_needed"},"error":{"code":"lock_timeout","message":"m"}}"#,
        ),
        (
            Outbound::Result(Box::new(Outcome {
                seq: 4,
                state: RequestState::Unknown,
                delivery: Delivery::Partial {
                    issued_units: 2,
                    reason: "focus_changed".into(),
                },
                observation: Observation::NotRequested,
                verification: Verification::Satisfied {
                    predicate: "value_equals".into(),
                },
                cleanup: Cleanup::Completed,
                error: None,
                data: None,
            })),
            r#"{"type":"result","seq":4,"state":"unknown","delivery":{"state":"partial","issuedUnits":2,"reason":"focus_changed"},"observation":{"state":"not_requested"},"verification":{"state":"satisfied","predicate":"value_equals"},"cleanup":{"state":"completed"}}"#,
        ),
        (
            Outbound::Result(Box::new(Outcome {
                seq: 5,
                state: RequestState::Unknown,
                delivery: Delivery::Submitted { issued_units: 2 },
                observation: Observation::NotRequested,
                verification: Verification::NotRequested,
                cleanup: Cleanup::Incomplete {
                    reason: "key_up_failed".into(),
                },
                error: None,
                data: None,
            })),
            r#"{"type":"result","seq":5,"state":"unknown","delivery":{"state":"submitted","issuedUnits":2},"observation":{"state":"not_requested"},"verification":{"state":"not_requested"},"cleanup":{"state":"incomplete","reason":"key_up_failed"}}"#,
        ),
        (
            Outbound::ProtocolError {
                message: "frame exceeds limit".into(),
            },
            r#"{"type":"protocol_error","message":"frame exceeds limit"}"#,
        ),
    ];
    for (frame, expected) in fixtures {
        assert_eq!(line(&frame), format!("{expected}\n"));
    }
}

#[test]
fn oversized_result_keeps_axes_and_reports_output_too_large() {
    let outcome = Outcome {
        seq: 9,
        state: RequestState::Completed,
        delivery: Delivery::ReadOnly,
        observation: Observation::Observed {
            snapshot: "E/C/1".into(),
            coverage: Coverage::default(),
        },
        verification: Verification::NotRequested,
        cleanup: Cleanup::NotNeeded,
        error: None,
        data: Some(serde_json::json!({ "blob": "x".repeat(MAX_FRAME_BYTES) })),
    };
    let bytes = encode(&Outbound::Result(Box::new(outcome))).unwrap();
    assert!(bytes.len() <= MAX_FRAME_BYTES + 1);
    let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(json["seq"], 9);
    assert_eq!(json["state"], "completed");
    assert_eq!(json["delivery"]["state"], "read_only");
    assert_eq!(json["observation"]["reason"], OUTPUT_TOO_LARGE);
    assert_eq!(json["error"]["code"], OUTPUT_TOO_LARGE);
    assert!(json.get("data").is_none());
    // Non-result frames are never cut: the caller treats them as a transport failure.
    assert!(
        encode(&Outbound::ProtocolError {
            message: "x".repeat(MAX_FRAME_BYTES),
        })
        .is_none()
    );
    assert!(
        diagnostic("가".repeat(MAX_FRAME_BYTES)).len() <= MAX_DIAGNOSTIC_BYTES + '…'.len_utf8()
    );
}

// ---- Properties (CONVENTIONS §6: parsers are proptest targets, zero panics). ----

fn text() -> impl Strategy<Value = String> {
    proptest::collection::vec(any::<char>(), 0..24).prop_map(String::from_iter)
}

fn target() -> impl Strategy<Value = Target> {
    (text(), text()).prop_map(|(snapshot, element)| Target { snapshot, element })
}

fn op() -> impl Strategy<Value = Op> {
    prop_oneof![
        4 => leaf_op(),
        1 => proptest::collection::vec(leaf_op(), 0..6).prop_map(|steps| Op::Batch { steps }),
    ]
}

fn leaf_op() -> impl Strategy<Value = Op> {
    prop_oneof![
        Just(Op::Capabilities),
        Just(Op::Permissions),
        Just(Op::Apps),
        (any::<i32>(), any::<Option<usize>>(), any::<Option<usize>>())
            .prop_map(|(pid, window, limit)| Op::Observe { pid, window, limit }),
        (target(), text()).prop_map(|(target, name)| Op::Action { target, name }),
        (target(), text(), any::<bool>()).prop_map(|(target, text, verify)| Op::SetValue {
            target,
            text,
            verify
        }),
        target().prop_map(|target| Op::Click { target }),
        (target(), text()).prop_map(|(target, text)| Op::TypeText { target, text }),
    ]
}

fn request() -> impl Strategy<Value = RequestFrame> {
    (text(), text(), any::<u64>(), any::<u64>(), op()).prop_map(
        |(epoch, context, seq, deadline_ms, op)| RequestFrame {
            epoch,
            context,
            seq,
            deadline_ms,
            op,
        },
    )
}

fn request_line(frame: &RequestFrame) -> Vec<u8> {
    let mut value = serde_json::to_value(frame).unwrap();
    value["type"] = serde_json::json!("request");
    serde_json::to_vec(&value).unwrap()
}

proptest::proptest! {
    #[test]
    fn arbitrary_bytes_never_panic_and_frames_stay_bounded(
        bytes in proptest::collection::vec(any::<u8>(), 0..4096)
    ) {
        let mut input = bytes.as_slice();
        let mut buf = Vec::new();
        let mut frames = 0;
        // Shorter than the limit, so the reader never errors; it only splits lines.
        while read_frame(&mut input, &mut buf).unwrap() {
            prop_assert!(buf.len() <= MAX_FRAME_BYTES);
            prop_assert!(!buf.contains(&b'\n'));
            if let Ok(Parsed::Rejected { message, .. }) = parse_line(&buf) {
                prop_assert!(message.len() <= MAX_DIAGNOSTIC_BYTES + '…'.len_utf8());
            }
            frames += 1;
        }
        let newlines = bytes.split(|&b| b == b'\n').count() - 1;
        let tail = usize::from(!bytes.is_empty() && bytes.last() != Some(&b'\n'));
        prop_assert_eq!(frames, newlines + tail);
    }

    #[test]
    fn valid_requests_round_trip_with_stable_digest(frame in request()) {
        let line = request_line(&frame);
        prop_assert!(!line.contains(&b'\n'));
        let parsed = parse_line(&line).unwrap();
        prop_assert_eq!(&parsed, &Parsed::Inbound(Inbound::Request(frame.clone())));
        if let Parsed::Inbound(Inbound::Request(back)) = parsed {
            prop_assert_eq!(back.digest(), frame.digest());
            // Validation is total: it answers, never panics.
            drop(back.op.validate());
        }
    }

    #[test]
    fn truncated_valid_frames_are_never_accepted(frame in request(), cut in any::<proptest::sample::Index>()) {
        let line = request_line(&frame);
        let prefix = &line[..cut.index(line.len())];
        prop_assert!(!matches!(parse_line(prefix), Ok(Parsed::Inbound(_))));
    }

    #[test]
    fn controls_round_trip(seq in any::<u64>()) {
        prop_assert_eq!(
            parse_line(format!(r#"{{"type":"status","seq":{seq}}}"#).as_bytes()).unwrap(),
            Parsed::Inbound(Inbound::Status { seq })
        );
        prop_assert_eq!(
            parse_line(format!(r#"{{"type":"cancel","seq":{seq}}}"#).as_bytes()).unwrap(),
            Parsed::Inbound(Inbound::Cancel { seq })
        );
    }

    #[test]
    fn every_encoded_frame_is_one_bounded_line(seq in any::<u64>(), message in text()) {
        for frame in [
            Outbound::Rejected { seq: Some(seq), code: RejectCode::Busy, message: message.clone() },
            Outbound::Result(Box::new(Outcome::not_dispatched(seq, "cancelled", message.clone()))),
        ] {
            let bytes = encode(&frame).unwrap();
            prop_assert!(bytes.len() <= MAX_FRAME_BYTES + 1);
            prop_assert!(!bytes[..bytes.len() - 1].contains(&b'\n'));
            prop_assert_eq!(bytes.last(), Some(&b'\n'));
        }
    }
}
