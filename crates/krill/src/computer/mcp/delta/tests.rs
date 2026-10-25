use super::*;
use proptest::prelude::*;

fn hello() -> Hello {
    Hello::parse(crate::computer::mcp::tests::hello()).unwrap()
}

fn args(format: &str, base: Option<&str>) -> Value {
    let mut args = json!({"format":format,"op":{"kind":"observe","pid":42,"window":0}});
    if let Some(base) = base {
        args["baseSnapshot"] = json!(base);
    }
    args
}

fn frame(generation: u64, elements: &Value) -> Value {
    let snapshot = format!("E/C/{generation}");
    json!({"type":"result","seq":generation,"state":"completed",
        "delivery":{"state":"read_only"},"observation":{"state":"observed","snapshot":snapshot,
        "coverage":{"nodes":elements.as_array().unwrap().len(),"truncated":false,"reasons":[]}},
        "verification":{"state":"not_requested"},"cleanup":{"state":"not_needed"},
        "data":{"snapshot":snapshot,"app":{"pid":42,"startWitness":"1.000000","bundleId":"fixture"},
            "window":{"index":0},"elements":elements}})
}

fn reconstruct(base: &Value, delta: &Value) -> Value {
    if delta["data"]["format"] == "full" {
        let mut result = delta.clone();
        let data = result["data"].as_object_mut().unwrap();
        data.remove("format");
        data.remove("deltaFallback");
        return result;
    }
    let mut nodes: BTreeMap<String, Value> = base["data"]["elements"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| (v["key"].as_str().unwrap().to_owned(), v.clone()))
        .collect();
    for key in delta["data"]["removed"].as_array().unwrap() {
        nodes.remove(key.as_str().unwrap());
    }
    for node in delta["data"]["upsert"].as_array().unwrap() {
        nodes.insert(node["key"].as_str().unwrap().to_owned(), node.clone());
    }
    let mut result = delta.clone();
    let data = result["data"].as_object_mut().unwrap();
    let order = data.remove("order").unwrap();
    let ordered: Vec<_> = order
        .as_array()
        .unwrap()
        .iter()
        .map(|key| nodes.remove(key.as_str().unwrap()).unwrap())
        .collect();
    assert!(nodes.is_empty());
    for field in [
        "format",
        "baseSnapshot",
        "elementCount",
        "upsert",
        "removed",
    ] {
        data.remove(field);
    }
    data.insert("elements".into(), json!(ordered));
    result
}

#[test]
fn validates_combinations_epoch_context_and_schema() {
    let hello = hello();
    assert_eq!(
        format(&args("delta", Some("E/C/1")), &hello).unwrap(),
        Format::Delta
    );
    for args in [
        args("delta", None),
        args("compact", Some("E/C/1")),
        args("full", Some("E/C/1")),
        args("delta", Some("X/C/1")),
        args("delta", Some("E/X/1")),
        args("delta", Some("E/C/01")),
        args("delta", Some("E/C/0")),
        args("delta", Some("E/C/1/2")),
        json!({"format":"delta","baseSnapshot":"E/C/1","op":{"kind":"click"}}),
        json!({"format":"delta","baseSnapshot":null,"op":{"kind":"observe"}}),
    ] {
        assert!(format(&args, &hello).is_err(), "{args}");
    }
    let tools = hello.tools();
    let execute = &tools["tools"][1]["inputSchema"];
    assert_eq!(
        execute["properties"]["format"]["enum"],
        json!(["compact", "full", "delta"])
    );
    assert_eq!(
        execute["oneOf"][1]["required"],
        json!(["format", "baseSnapshot"])
    );
    assert_eq!(
        execute["oneOf"][1]["properties"]["op"]["properties"]["kind"]["const"],
        "observe"
    );
}

#[test]
fn reconstructs_all_fields_order_new_state_and_display_removals() {
    let mut cache = Bases::default();
    let hello = hello();
    let before = frame(
        1,
        &json!([{"key":"e0","role":"AXWindow","value":23},
        {"key":"e1","role":"AXTextField","value":"old","frame":{"x":1}},
        {"key":"e2","role":"AXButton","enabled":true}]),
    );
    cache
        .render(before.clone(), &args("full", None), &hello)
        .unwrap();
    let after = frame(
        2,
        &json!([{"key":"e1","role":"AXTextField","value":"new","focused":true},
        {"key":"e0","role":"AXWindow","value":23},
        {"key":"e3","role":"AXButton","actions":["AXPress"]}]),
    );
    let delta = cache
        .render(after.clone(), &args("delta", Some("E/C/1")), &hello)
        .unwrap();
    assert_eq!(delta["data"]["format"], "delta");
    assert_eq!(delta["data"]["removed"], json!(["e2"]));
    assert_eq!(delta["data"]["upsert"].as_array().unwrap().len(), 2);
    assert_eq!(reconstruct(&before, &delta), after);
    let third = frame(3, &after["data"]["elements"]);
    let chained = cache
        .render(third.clone(), &args("delta", Some("E/C/2")), &hello)
        .unwrap();
    assert_eq!(chained["data"]["upsert"], json!([]));
    assert_eq!(reconstruct(&after, &chained), third);
}

#[test]
fn incomplete_unavailable_compact_and_changed_scope_bases_fall_back_without_gone() {
    let hello = hello();
    for (change, reason) in [
        ("missing", "base_missing_or_evicted"),
        ("base", "base_partial"),
        ("current", "current_partial"),
        ("compact", "base_requires_full_observation"),
        ("scope", "observation_scope_changed"),
        ("flags", "current_partial"),
    ] {
        let mut cache = Bases::default();
        let mut before = frame(1, &json!([{"key":"e0"},{"key":"e1"}]));
        let mut after = frame(2, &json!([{"key":"e0"}]));
        if change == "base" {
            before["observation"]["coverage"]["truncated"] = json!(true);
        }
        if change == "current" {
            after["observation"]["coverage"]["truncated"] = json!(true);
        }
        if change == "flags" {
            after["data"]["elements"][0]["childrenTruncated"] = json!(true);
        }
        if change == "scope" {
            after["data"]["app"]["startWitness"] = json!("2.000000");
        }
        if change != "missing" {
            cache
                .render(
                    before,
                    &args(
                        if change == "compact" {
                            "compact"
                        } else {
                            "full"
                        },
                        None,
                    ),
                    &hello,
                )
                .unwrap();
        }
        let result = cache
            .render(after.clone(), &args("delta", Some("E/C/1")), &hello)
            .unwrap();
        assert_eq!(result["data"]["format"], "compact");
        assert_eq!(result["data"]["deltaFallback"]["reason"], reason);
        assert!(result["data"].get("removed").is_none());
        assert_eq!(result["observation"], after["observation"]);
    }
}

#[test]
fn identity_mismatch_reuse_and_eviction_never_alias_a_base() {
    let hello = hello();
    let mut cache = Bases::default();
    for generation in 1..=6 {
        cache
            .render(
                frame(generation, &json!([{"key":"e0"}])),
                &args("full", None),
                &hello,
            )
            .unwrap();
        assert!(cache.entries.len() <= MAX_BASES);
    }
    let result = cache
        .render(
            frame(7, &json!([{"key":"e0"}])),
            &args("delta", Some("E/C/1")),
            &hello,
        )
        .unwrap();
    assert_eq!(
        result["data"]["deltaFallback"]["reason"],
        "base_missing_or_evicted"
    );
    for mut bad in [
        frame(7, &json!([{"key":"e0"}])),
        frame(8, &json!([{"key":"e0"}])),
    ] {
        if bad["seq"] == 8 {
            bad["data"]["snapshot"] = json!("E/foreign/8");
        }
        assert!(cache.render(bad, &args("full", None), &hello).is_err());
    }
    let mut bad = frame(8, &json!([{"key":"e0"}]));
    bad["observation"]["snapshot"] = json!("E/C/9");
    assert!(cache.render(bad, &args("full", None), &hello).is_err());
    assert!(
        cache
            .render(
                frame(9, &json!([{"key":"e0"},{"key":"e0"}])),
                &args("full", None),
                &hello
            )
            .is_err()
    );
}

#[test]
fn serialized_cache_budget_and_oversize_delivery_are_explicit() {
    let hello = hello();
    let mut cache = Bases::default();
    for generation in 1..=10 {
        let value = frame(
            generation,
            &json!([{"key":"e0","value":"x".repeat(700_000)}]),
        );
        cache.render(value, &args("full", None), &hello).unwrap();
        assert!(cache.bytes <= MAX_BASE_BYTES);
        assert!(cache.entries.len() <= MAX_BASES);
        assert_eq!(
            cache.bytes,
            cache.entries.iter().map(|e| e.bytes).sum::<usize>()
        );
    }
    // Oversized full output cannot be delivered exactly, so it retains only markers.
    let mut cache = Bases::default();
    for generation in 1..=5 {
        cache
            .render(
                frame(
                    generation,
                    &json!([{"key":"e0","value":"x".repeat(1_100_000)}]),
                ),
                &args("full", None),
                &hello,
            )
            .unwrap();
    }
    assert_eq!(cache.entries.len(), MAX_BASES);
    assert!(cache.bytes < 1024);
    assert!(cache.entries.iter().all(|base| base.data.is_none()));
    let result = cache
        .render(
            frame(6, &json!([{"key":"e0"}])),
            &args("delta", Some("E/C/5")),
            &hello,
        )
        .unwrap();
    assert_eq!(
        result["data"]["deltaFallback"]["reason"],
        "base_requires_full_observation"
    );
}

proptest! {
    #[test]
    fn arbitrary_display_add_remove_change_and_reorder_reconstruct_exactly(
        before in prop::collection::btree_map(0u16..60, ".{0,20}", 0..40),
        after in prop::collection::btree_map(0u16..60, ".{0,20}", 0..40),
        reverse in any::<bool>(),
    ) {
        let elements = |map: &BTreeMap<u16, String>| -> Vec<Value> {
            map.iter().map(|(key, value)| json!({"key":format!("e{key}"),"value":value,"enabled":key%2==0})).collect()
        };
        let base = frame(1, &json!(elements(&before)));
        let mut changed = elements(&after);
        if reverse { changed.reverse(); }
        let current = frame(2, &json!(changed));
        let mut cache = Bases::default();
        let hello = hello();
        cache.render(base.clone(), &args("full", None), &hello).unwrap();
        let result = cache.render(current.clone(), &args("delta", Some("E/C/1")), &hello).unwrap();
        prop_assert_eq!(reconstruct(&base, &result), current);
    }
}

#[test]
fn oversized_delta_uses_the_deliverable_full_fresh_observation() {
    let hello = hello();
    let mut cache = Bases::default();
    let elements = |letter: &str| -> Value {
        json!(
            (0..2000)
                .map(|n| json!({"key":format!("e{n}"),"value":letter.repeat(490)}))
                .collect::<Vec<_>>()
        )
    };
    let before = frame(1, &elements("x"));
    assert!(serde_json::to_vec(&before).unwrap().len() < MAX_FRAME - 2048);
    cache.render(before, &args("full", None), &hello).unwrap();
    let after = frame(2, &elements("y"));
    let result = cache
        .render(after.clone(), &args("delta", Some("E/C/1")), &hello)
        .unwrap();
    assert_eq!(result["data"]["format"], "full");
    assert_eq!(
        result["data"]["deltaFallback"]["reason"],
        "delta_not_smaller_than_full"
    );
    assert_eq!(result["observation"], after["observation"]);
    assert_eq!(result["data"]["snapshot"], "E/C/2");
    assert!(result["data"].get("removed").is_none());
    assert_eq!(result["data"]["elements"], after["data"]["elements"]);
    assert!(serde_json::to_vec(&result).unwrap().len() < MAX_FRAME - 2048);
    assert!(cache.entries.back().unwrap().data.is_some());
}

#[test]
fn change_heavy_full_fallback_is_an_exact_base_for_the_next_delta() {
    let hello = hello();
    let mut cache = Bases::default();
    let elements = |text: &str| {
        json!(
            (0..64)
                .map(|n| json!({"key":format!("e{n}"),"value":text.repeat(100),"enabled":true}))
                .collect::<Vec<_>>()
        )
    };
    let before = frame(1, &elements("before"));
    cache
        .render(before.clone(), &args("full", None), &hello)
        .unwrap();
    let after = frame(2, &elements("after"));
    let full = cache
        .render(after.clone(), &args("delta", Some("E/C/1")), &hello)
        .unwrap();
    assert_eq!(full["data"]["format"], "full");
    assert_eq!(
        full["data"]["deltaFallback"]["reason"],
        "delta_not_smaller_than_full"
    );
    assert_eq!(reconstruct(&before, &full), after);
    let mut next = frame(3, &after["data"]["elements"]);
    next["data"]["elements"][0]["value"] = json!("one changed value");
    let delta = cache
        .render(next.clone(), &args("delta", Some("E/C/2")), &hello)
        .unwrap();
    assert_eq!(delta["data"]["format"], "delta");
    assert_eq!(delta["data"]["upsert"].as_array().unwrap().len(), 1);
    assert_eq!(reconstruct(&after, &delta), next);
}

#[test]
fn large_default_compact_and_partial_observations_keep_only_tiny_markers() {
    let hello = hello();
    let mut cache = Bases::default();
    for generation in 1..=20 {
        let mut current = frame(
            generation,
            &json!([{"key":"e0","value":"large-secret-value".repeat(40_000)}]),
        );
        let requested = if generation % 2 == 0 {
            "full"
        } else {
            "compact"
        };
        if requested == "full" {
            current["observation"]["coverage"]["truncated"] = json!(true);
        }
        cache
            .render(current, &args(requested, None), &hello)
            .unwrap();
        assert!(cache.entries.len() <= MAX_BASES);
        assert!(cache.bytes < 1024);
        assert!(cache.entries.iter().all(|base| base.data.is_none()));
        let exact: usize = cache
            .entries
            .iter()
            .map(|base| {
                serde_json::to_vec(&json!({
                    "snapshot":base.snapshot,"complete":base.complete,"reconstructable":false
                }))
                .unwrap()
                .len()
            })
            .sum();
        assert_eq!(cache.bytes, exact);
    }
    let compact = cache
        .render(
            frame(21, &json!([{"key":"e0"}])),
            &args("delta", Some("E/C/19")),
            &hello,
        )
        .unwrap();
    assert_eq!(
        compact["data"]["deltaFallback"]["reason"],
        "base_requires_full_observation"
    );
    let partial = cache
        .render(
            frame(22, &json!([{"key":"e0"}])),
            &args("delta", Some("E/C/20")),
            &hello,
        )
        .unwrap();
    assert_eq!(partial["data"]["deltaFallback"]["reason"], "base_partial");
}

#[test]
fn storage_byte_cap_evicts_oldest_before_count_and_rejects_oversized_entry() {
    // Exercise the storage bound independently of the stricter native frame bound.
    let mut cache = Bases::default();
    for generation in 1..=5 {
        cache
            .retain(
                format!("E/C/{generation}"),
                Some(json!({"value":"x".repeat(1_100_000)})),
                true,
            )
            .unwrap();
        assert!(cache.bytes <= MAX_BASE_BYTES);
        assert_eq!(
            cache.bytes,
            cache.entries.iter().map(|e| e.bytes).sum::<usize>()
        );
    }
    assert_eq!(cache.entries.len(), 3);
    assert_eq!(cache.entries.front().unwrap().snapshot, "E/C/3");
    let bytes = cache.bytes;
    cache
        .retain(
            "E/C/6".into(),
            Some(json!({"value":"x".repeat(MAX_BASE_BYTES)})),
            true,
        )
        .unwrap();
    assert_eq!(cache.bytes, bytes);
    assert_eq!(cache.entries.len(), 3);
}
