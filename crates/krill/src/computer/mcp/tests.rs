use std::io::{self, Cursor};

use proptest::prelude::*;
use serde_json::{Value, json};

use super::schema::{CONTRACT, Hello, VERSION, is_error, tool_result, unknown};
use super::transport::{MAX_FRAME, encode, read_line};
use super::{options, request_key};

fn hello() -> Value {
    json!({"type":"hello","contract":CONTRACT,"supported":true,"pid":123,
        "epoch":"E","context":"C","capabilities":["capabilities","permissions","apps","observe","action","set_value","click","type_text"],
        "unsupported":["batch","clipboard","screenshot"],
        "limits":{"maxFrameBytes":MAX_FRAME,"maxDeadlineMs":30000,"maxSeqGap":1024,
            "maxTextUtf16":4096,"maxObserveNodes":2000,"queueCapacity":4,"ledgerCapacity":64}})
}

#[test]
fn experimental_opt_in_and_absolute_worker_are_required() {
    for args in [
        vec![],
        vec!["--worker", "/missing"],
        vec!["--experimental", "--worker", "relative"],
        vec!["--experimental", "--experimental"],
        vec!["--experimental", "--unknown"],
    ] {
        assert!(options(&args.into_iter().map(str::to_owned).collect::<Vec<_>>()).is_err());
    }
}

#[test]
fn hello_requires_supported_known_contract_and_bounded_limits() {
    assert!(Hello::parse(hello()).is_ok());
    for (path, bad) in [
        (vec!["contract"], json!("krill-computer-worker/2")),
        (vec!["supported"], json!(false)),
        (vec!["epoch"], json!("")),
        (vec!["context"], json!("C/other")),
        (vec!["pid"], json!(0)),
        (vec!["limits", "maxFrameBytes"], json!(MAX_FRAME + 1)),
        (vec!["limits", "maxDeadlineMs"], json!(0)),
    ] {
        let mut value = hello();
        if path.len() == 1 {
            value[path[0]] = bad;
        } else {
            value[path[0]][path[1]] = bad;
        }
        assert!(Hello::parse(value).is_err(), "{path:?}");
    }
}

#[test]
fn foreign_mutation_targets_and_invented_ops_fail_before_transport() {
    let hello = Hello::parse(hello()).unwrap();
    let valid = json!({"kind":"click","target":{"snapshot":"E/C/1","element":"e2"}});
    assert!(hello.validate_op(&valid).is_ok());
    let mut window_target = valid.clone();
    window_target["target"]["element"] = json!("e0");
    assert!(hello.validate_op(&window_target).is_ok());
    for element in ["e01", "e-1", "e+1", "e", "1"] {
        window_target["target"]["element"] = json!(element);
        assert!(hello.validate_op(&window_target).is_err());
    }
    for snapshot in ["old/C/1", "E/old/1", "E/C/0", "E/C/1/2", "E/C", "E/C/-1"] {
        let mut op = valid.clone();
        op["target"]["snapshot"] = json!(snapshot);
        assert!(hello.validate_op(&op).is_err());
    }
    for op in [
        json!({"kind":"clipboard"}),
        json!({"kind":"batch","steps":[]}),
        json!({"kind":"apps","epoch":"replacement"}),
        json!({"kind":"observe","pid":-1}),
        json!({"kind":"observe","pid":1,"limit":2001}),
    ] {
        assert!(hello.validate_op(&op).is_err(), "{op}");
    }
}

#[test]
fn native_utf16_limit_is_enforced_instead_of_byte_or_scalar_length() {
    let hello = Hello::parse(hello()).unwrap();
    let mut op = json!({"kind":"set_value","target":{"snapshot":"E/C/1","element":"e1"},
        "text":"😀".repeat(2048),"verify":true});
    assert!(hello.validate_op(&op).is_ok());
    op["text"] = json!("😀".repeat(2049));
    assert!(hello.validate_op(&op).is_err());
}

#[test]
fn advertised_tools_require_caller_sequence_and_never_offer_unimplemented_ops() {
    let hello = Hello::parse(hello()).unwrap();
    let tools = hello.tools();
    let execute = &tools["tools"][1];
    assert_eq!(execute["name"], "computer_execute");
    assert_eq!(
        execute["inputSchema"]["required"],
        json!(["seq", "deadlineMs", "op"])
    );
    assert_eq!(execute["inputSchema"]["additionalProperties"], false);
    assert!(!tools.to_string().contains("\"batch\""));
    assert_eq!(VERSION, "2025-11-25");
}

#[test]
fn independent_error_axes_do_not_erase_submitted_delivery() {
    let base = json!({"seq":7,"state":"completed","delivery":{"state":"submitted","issuedUnits":2},
        "observation":{"state":"not_requested"},"verification":{"state":"not_requested"},"cleanup":{"state":"completed"}});
    assert!(!is_error(&base));
    for (axis, state) in [
        ("observation", "failed"),
        ("verification", "unknown"),
        ("verification", "unsatisfied"),
        ("cleanup", "incomplete"),
    ] {
        let mut result = base.clone();
        result[axis]["state"] = json!(state);
        let wrapped = tool_result(result);
        assert_eq!(wrapped["isError"], true);
        assert_eq!(
            wrapped["structuredContent"]["delivery"]["state"],
            "submitted"
        );
    }
    let unknown = unknown(7, "worker EOF");
    assert!(is_error(&unknown));
    assert_eq!(unknown["seq"], 7);
    assert_eq!(unknown["delivery"]["state"], "unknown");
}

#[test]
fn giant_observation_is_not_duplicated_and_envelope_trimming_preserves_delivery() {
    let result = tool_result(json!({"type":"result","seq":1,"state":"completed",
        "delivery":{"state":"submitted","issuedUnits":1},"data":{"tree":"x".repeat(MAX_FRAME)},
        "observation":{"state":"observed","snapshot":"E/C/1"},"verification":{"state":"not_requested"},"cleanup":{"state":"completed"}}));
    assert!(encode(&result).is_ok());
    assert_eq!(
        result["structuredContent"]["delivery"]["state"],
        "submitted"
    );
    assert!(result["structuredContent"].get("data").is_none());
    assert_eq!(
        result["structuredContent"]["error"]["code"],
        "output_too_large"
    );
    assert!(result["content"][0]["text"].as_str().unwrap().len() < 256);
    assert_eq!(result["isError"], true);
}

#[test]
fn exact_frame_boundary_and_oversize_refusal_are_deterministic() {
    let mut bytes = Vec::new();
    let mut exact = vec![b' '; MAX_FRAME];
    exact.push(b'\n');
    assert!(read_line(&mut Cursor::new(exact), &mut bytes).unwrap());
    assert_eq!(bytes.len(), MAX_FRAME);
    assert!(read_line(&mut Cursor::new(vec![b' '; MAX_FRAME + 1]), &mut bytes).is_err());
    assert!(!read_line(&mut Cursor::new(Vec::<u8>::new()), &mut bytes).unwrap());
    // A syntactically complete body without its framing delimiter is not a committed result.
    assert!(read_line(&mut Cursor::new(br#"{"type":"result"}"#), &mut bytes).is_err());
}

#[test]
fn rpc_ids_preserve_string_number_distinction_and_refuse_null_or_fraction() {
    assert_ne!(request_key(&json!(1)), request_key(&json!("1")));
    assert!(request_key(&Value::Null).is_none());
    assert!(request_key(&json!(1.5)).is_none());
    assert!(request_key(&json!("x".repeat(257))).is_none());
}

proptest! {
    #[test]
    fn arbitrary_utf8_is_one_ndjson_frame(text in ".{0,4096}") {
        let value = json!({"jsonrpc":"2.0","id":"id","result":{"text":text}});
        let bytes = encode(&value).unwrap();
        // Exactly one delimiter: the first newline is the final byte.
        prop_assert_eq!(bytes.iter().position(|b| *b == b'\n'), Some(bytes.len() - 1));
        let mut framed = Vec::new();
        prop_assert!(read_line(&mut Cursor::new(bytes), &mut framed).unwrap());
        prop_assert_eq!(serde_json::from_slice::<Value>(&framed).unwrap(), value);
    }

    #[test]
    fn arbitrary_input_frames_and_hello_cannot_panic(bytes in prop::collection::vec(any::<u8>(), 0..8192)) {
        // Bodies stay below MAX_FRAME, so only EOF framing decides the outcome: empty input is a
        // clean end, a delimited line is a frame, and an unterminated tail is refused, never parsed.
        let terminated = bytes.contains(&b'\n');
        let empty = bytes.is_empty();
        let mut framed = Vec::new();
        match read_line(&mut Cursor::new(bytes), &mut framed) {
            Ok(true) => {
                prop_assert!(terminated);
                if let Ok(value) = serde_json::from_slice::<Value>(&framed) {
                    Hello::parse(value).ok();
                }
            }
            Ok(false) => prop_assert!(empty),
            Err(error) => {
                prop_assert!(!terminated);
                prop_assert!(!empty);
                prop_assert_eq!(error.kind(), io::ErrorKind::UnexpectedEof);
            }
        }
    }

    #[test]
    fn mutation_never_accepts_foreign_epoch(epoch in "[A-Za-z0-9]{1,32}") {
        let hello = Hello::parse(hello()).unwrap();
        let op = json!({"kind":"click","target":{"snapshot":format!("{epoch}/C/1"),"element":"e1"}});
        prop_assert_eq!(hello.validate_op(&op).is_ok(), epoch == "E");
    }
}

fn retain_completed(store: &mut super::retention::Retention, seq: u64, text: &str) {
    use super::retention::{Recorded, Sequence};
    use std::time::{Duration, Instant};
    let key = format!("n:{seq}");
    let input = json!({"jsonrpc":"2.0","id":seq,"params":{"seq":seq,"text":text}});
    let args = json!({"seq":seq,"text":text});
    let response = json!({"jsonrpc":"2.0","id":seq,"result":{"seq":seq}});
    let result = json!({"seq":seq});
    let input_bytes = serde_json::to_vec(&input).unwrap().len();
    let args_bytes = serde_json::to_vec(&args).unwrap().len();
    let response_bytes = serde_json::to_vec(&response).unwrap().len();
    let result_bytes = serde_json::to_vec(&result).unwrap().len();
    assert!(store.room(input_bytes, 0, 1, 0));
    let order = store.next_order();
    store.input_bytes += input_bytes;
    store.requests.insert(
        key.clone(),
        Recorded {
            input,
            response: None,
            queued: false,
            input_bytes,
            response_bytes: 0,
            order,
        },
    );
    assert!(store.room(args_bytes, 0, 0, 1));
    let order = store.next_order();
    store.input_bytes += args_bytes;
    store.sequences.insert(
        seq,
        Sequence {
            args,
            until: Instant::now() + Duration::from_secs(1),
            waiting: vec![key.clone()],
            result: None,
            args_bytes,
            result_bytes: 0,
            order,
        },
    );
    store.highwater = seq;
    assert!(store.room(0, result_bytes, 0, 0));
    let state = store.sequences.get_mut(&seq).unwrap();
    state.result = Some(result);
    state.result_bytes = result_bytes;
    state.waiting.clear();
    store.cache_bytes += result_bytes;
    assert!(store.room(0, response_bytes, 0, 0));
    let record = store.requests.get_mut(&key).unwrap();
    record.response = Some(response);
    record.response_bytes = response_bytes;
    record.queued = true;
    store.cache_bytes += response_bytes;
}

#[test]
fn rolling_retention_outlives_twice_each_capacity_without_forgetting_highwater() {
    use super::retention::{
        MAX_CACHE_BYTES, MAX_INPUT_BYTES, MAX_REQUESTS, MAX_SEQUENCES, Retention,
    };
    let mut store = Retention::default();
    let count = u64::try_from(MAX_REQUESTS * 3).unwrap();
    for seq in 1..=count {
        retain_completed(&mut store, seq, "small operation");
        assert!(store.requests.len() <= MAX_REQUESTS);
        assert!(store.sequences.len() <= MAX_SEQUENCES);
        assert!(store.input_bytes <= MAX_INPUT_BYTES);
        assert!(store.cache_bytes <= MAX_CACHE_BYTES);
        assert_eq!(
            store.input_bytes,
            store
                .requests
                .values()
                .map(|r| r.input_bytes)
                .sum::<usize>()
                + store
                    .sequences
                    .values()
                    .map(|s| s.args_bytes)
                    .sum::<usize>()
        );
        assert_eq!(
            store.cache_bytes,
            store
                .requests
                .values()
                .map(|r| r.response_bytes)
                .sum::<usize>()
                + store
                    .sequences
                    .values()
                    .map(|s| s.result_bytes)
                    .sum::<usize>()
        );
    }
    assert!(!store.requests.contains_key("n:1"));
    assert!(!store.sequences.contains_key(&1));
    assert_eq!(store.highwater, count);
    // Missing payload is not an unseen seq: the proxy's execute admission must refuse replay.
    assert!(1 <= store.highwater);
}

#[test]
fn rolling_byte_pressure_preserves_pending_entries_and_frees_completed_bytes() {
    use super::retention::{MAX_INPUT_BYTES, Recorded, Retention, Sequence};
    use std::time::{Duration, Instant};
    let mut store = Retention::default();
    store.requests.insert(
        "pending".into(),
        Recorded {
            input: json!({}),
            response: None,
            queued: false,
            input_bytes: 2,
            response_bytes: 0,
            order: 0,
        },
    );
    store.sequences.insert(
        999,
        Sequence {
            args: json!({}),
            until: Instant::now() + Duration::from_secs(1),
            waiting: vec!["pending".into()],
            result: None,
            args_bytes: 2,
            result_bytes: 0,
            order: 0,
        },
    );
    store.input_bytes = 4;
    let text = "x".repeat(MAX_INPUT_BYTES / 8);
    for seq in 1..=20 {
        retain_completed(&mut store, seq, &text);
    }
    assert!(store.requests.contains_key("pending"));
    assert!(store.sequences.contains_key(&999));
    assert!(!store.sequences.contains_key(&1));
    assert!(store.input_bytes <= MAX_INPUT_BYTES);
    assert_eq!(
        store.input_bytes,
        store
            .requests
            .values()
            .map(|r| r.input_bytes)
            .sum::<usize>()
            + store
                .sequences
                .values()
                .map(|s| s.args_bytes)
                .sum::<usize>()
    );
}

#[test]
fn all_pending_sequence_slots_return_busy_without_eviction() {
    use super::retention::{MAX_SEQUENCES, Retention, Sequence};
    use std::time::{Duration, Instant};
    let mut store = Retention::default();
    for seq in 1..=u64::try_from(MAX_SEQUENCES).unwrap() {
        store.sequences.insert(
            seq,
            Sequence {
                args: json!({}),
                until: Instant::now() + Duration::from_secs(1),
                waiting: vec![seq.to_string()],
                result: None,
                args_bytes: 2,
                result_bytes: 0,
                order: seq,
            },
        );
        store.input_bytes += 2;
    }
    assert!(!store.room(2, 0, 0, 1));
    assert_eq!(store.sequences.len(), MAX_SEQUENCES);
    assert!(store.sequences.values().all(|s| s.result.is_none()));
}

#[test]
fn native_zero_window_and_advertised_short_batches_validate_without_index_conversion() {
    let mut raw = hello();
    raw["capabilities"]
        .as_array_mut()
        .unwrap()
        .push(json!("batch"));
    raw["unsupported"] = json!(["clipboard", "screenshot"]);
    raw["limits"]["maxBatchSteps"] = json!(4);
    let hello = Hello::parse(raw.clone()).unwrap();
    assert!(
        hello
            .validate_op(&json!({"kind":"observe","pid":7,"window":0}))
            .is_ok()
    );
    assert!(
        hello
            .validate_op(&json!({"kind":"observe","pid":7,"window":-1}))
            .is_err()
    );
    let step = json!({"kind":"click","target":{"snapshot":"E/C/1","element":"e0"}});
    assert!(
        hello
            .validate_op(&json!({"kind":"batch","steps":[step.clone(),step.clone()]}))
            .is_ok()
    );
    for steps in [
        json!([]),
        json!([step.clone(), step.clone(), step.clone(), step.clone(), step]),
        json!([{"kind":"apps"}]),
        json!([{"kind":"batch","steps":[]}]),
        json!([{"kind":"click","target":{"snapshot":"old/C/1","element":"e0"}}]),
    ] {
        assert!(
            hello
                .validate_op(&json!({"kind":"batch","steps":steps}))
                .is_err()
        );
    }
    assert!(hello.tools().to_string().contains("\"maxItems\":4"));
    raw["limits"]["maxBatchSteps"] = json!(5);
    assert!(Hello::parse(raw).is_err());
}

#[test]
fn known_partial_batch_is_error_but_unknown_or_incomplete_cleanup_closes_session() {
    use super::schema::must_close;
    let mut result = json!({"type":"result","seq":9,"state":"completed",
        "delivery":{"state":"partial","issuedUnits":2,"reason":"later_step_refused"},
        "observation":{"state":"not_requested"},"verification":{"state":"not_requested"},
        "cleanup":{"state":"completed"},"error":{"code":"batch_stopped","message":"second step refused"}});
    assert!(is_error(&result));
    assert!(!must_close(&result));
    result["cleanup"]["state"] = json!("incomplete");
    assert!(must_close(&result));
    result["cleanup"]["state"] = json!("completed");
    result["state"] = json!("unknown");
    assert!(must_close(&result));
    result["state"] = json!("completed");
    result["data"] = json!({"quiescing":true});
    assert!(must_close(&result));
}

#[test]
fn compact_observation_is_default_and_format_is_part_of_sequence_payload() {
    use super::schema::full_format;
    let base = json!({"seq":1,"deadlineMs":5000,"op":{"kind":"observe","pid":7}});
    assert!(!full_format(&base).unwrap());
    let mut full = base.clone();
    full["format"] = json!("full");
    assert!(full_format(&full).unwrap());
    assert_ne!(base, full);
    full["format"] = json!("compact");
    assert!(!full_format(&full).unwrap());
    for format in [Value::Null, json!(true), json!(1), json!("json")] {
        full["format"] = format;
        assert!(full_format(&full).is_err());
    }
}
