use super::*;
use gpui::TestAppContext;
use std::sync::Mutex;

fn page(slug: &str, next: Option<&str>) -> Page {
    serde_json::from_value(serde_json::json!({
        "artifacts": [{
            "artifact": {"slug": slug, "originalFileName": "계획.html", "expiresAt": "2026-11-23"},
            "shareUrl": format!("https://share.krill.page/a/{slug}"),
        }],
        "nextCursor": next,
    }))
    .unwrap()
}

#[gpui::test]
fn library_fetches_only_on_demand_and_retries_pagination_without_losing_items(
    cx: &mut TestAppContext,
) {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let seen = calls.clone();
    let call: Call = Arc::new(move |cursor| {
        let mut calls = seen.lock().unwrap();
        calls.push(cursor);
        match calls.len() {
            1 => Ok(page("one", Some("next page"))),
            2 => Err("offline".into()),
            3 => Ok(page("two", Some("next page"))),
            _ => Ok(page("fresh", None)),
        }
    });
    let view = cx.new(|cx| View::with_call(call, cx));
    cx.run_until_parked();
    view.read_with(cx, |v, _| {
        assert_eq!(v.items.len(), 1);
        assert_eq!(v.items[0].title(), "계획.html");
        assert!(v.items[0].matches("계획 one"));
        assert!(!v.items[0].matches("missing"));
    });
    cx.run_until_parked();
    assert_eq!(calls.lock().unwrap().len(), 1);
    view.update(cx, |v, cx| v.load(true, cx));
    cx.run_until_parked();
    view.read_with(cx, |v, _| {
        assert_eq!(v.error.as_deref(), Some("offline"));
        assert_eq!(v.items.len(), 1);
        assert_eq!(v.next.as_deref(), Some("next page"));
    });
    view.update(cx, |v, cx| v.load(true, cx));
    cx.run_until_parked();
    view.read_with(cx, |v, _| {
        assert!(v.error.is_none());
        assert_eq!(v.items.len(), 2);
        assert!(v.next.is_none());
    });
    view.update(cx, |v, cx| v.load(false, cx));
    cx.run_until_parked();
    view.read_with(cx, |v, _| assert_eq!(v.items[0].artifact.slug, "fresh"));
    assert_eq!(
        calls.lock().unwrap().as_slice(),
        &[
            None,
            Some("next page".into()),
            Some("next page".into()),
            None,
        ]
    );
}

#[cfg(unix)]
#[test]
fn list_uses_the_cli_state_and_passes_cursors_as_literal_arguments() {
    use std::os::unix::fs::PermissionsExt as _;
    let dir = std::env::temp_dir().join(format!("krill-artifact-view-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let script = dir.join("krill");
    std::fs::write(
        &script,
        r#"#!/bin/sh
[ "$1" = artifacts ] && [ "$2" = list ] && [ "$3" = --json ] || exit 8
[ "$4" = --cursor ] && [ "$5" = 'next & $(literal)' ] || exit 9
[ -d "$KRILL_STATE_DIR" ] || exit 10
printf '%s' '{"artifacts":[],"nextCursor":null}'
"#,
    )
    .unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700)).unwrap();
    let page = list_command(
        &mut Command::new(script),
        Some(&dir),
        Some("next & $(literal)".into()),
    )
    .unwrap();
    assert!(page.artifacts.is_empty());
    assert!(page.next_cursor.is_none());
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn malformed_responses_fail_and_non_web_urls_cannot_be_opened() {
    assert!(serde_json::from_str::<Page>(r#"{"unexpected":[]}"#).is_err());
    let mut page = page("one", None);
    assert!(page.artifacts[0].openable());
    page.artifacts[0].share_url = "file:///private/key".into();
    assert!(!page.artifacts[0].openable());
}

#[test]
fn display_labels_format_type_size_and_timestamps_like_orca() {
    let md_item: Item = serde_json::from_value(serde_json::json!({
        "artifact": {
            "slug": "spec-doc",
            "title": "Architecture Spec",
            "originalFileName": "spec.md",
            "sourceContentType": "text/markdown; charset=utf-8",
            "updatedAt": "2026-10-25T14:32:10Z",
            "expiresAt": "2026-11-25T00:00:00Z",
            "byteSize": 2048
        },
        "shareUrl": "https://share.krill.page/a/spec-doc"
    }))
    .unwrap();
    assert_eq!(md_item.type_label(), "Markdown");
    assert!(md_item.matches("markdown spec"));
    assert_eq!(format_byte_size(md_item.artifact.byte_size), "2.0 KB");
    assert_eq!(format_byte_size(Some(512)), "512 B");
    assert_eq!(format_byte_size(Some(15 * 1024)), "15 KB");
    assert_eq!(format_byte_size(None), "—");
    assert_eq!(
        format_timestamp_short(md_item.artifact.updated_at.as_deref()),
        crate::local_time::format_timestamp(
            krill_proto::time::parse_reset_epoch("2026-10-25T14:32:10Z").unwrap()
        )
        .unwrap()
    );
    assert_eq!(
        format_timestamp_short(Some("2026-10-25T14:32:10Z")),
        format_timestamp_short(Some("2026-10-25T23:32:10+09:00"))
    );
    assert_eq!(format_timestamp_short(Some("2026-11-23")), "2026-11-23");
    assert_eq!(format_timestamp_short(None), "—");
}

#[gpui::test]
fn retention_changes_preserve_link_and_keep_previous_state_on_failure(cx: &mut TestAppContext) {
    let requests = Arc::new(Mutex::new(Vec::new()));
    let seen = requests.clone();
    let view = cx.new(|cx| View::with_call(Arc::new(|_| Ok(page("one", None))), cx));
    cx.run_until_parked();
    view.update(cx, |v, cx| {
        v.retention_call = Arc::new(move |slug, days| {
            seen.lock().unwrap().push((slug, days));
            if days == 7 {
                return Err("offline".into());
            }
            let mut item = page("one", None).artifacts.remove(0);
            item.artifact.retention_days = Some(days);
            item.artifact.expires_at = None;
            Ok(item)
        });
        v.set_retention("one".into(), 0, cx);
    });
    cx.run_until_parked();
    view.read_with(cx, |v, _| {
        assert_eq!(v.items[0].artifact.retention_days, Some(0));
        assert_eq!(v.items[0].share_url, "https://share.krill.page/a/one");
        assert_eq!(
            retention::expiration_label(&v.items[0].artifact),
            text::RETENTION_NEVER.get()
        );
        assert!(v.retention_slug.is_none());
    });
    view.update(cx, |v, cx| v.set_retention("one".into(), 7, cx));
    cx.run_until_parked();
    view.read_with(cx, |v, _| {
        assert_eq!(v.error.as_deref(), Some("offline"));
        assert_eq!(v.items[0].artifact.retention_days, Some(0));
    });
    view.update(cx, |v, cx| {
        v.sharing_enabled = false;
        v.set_retention("one".into(), 30, cx);
    });
    cx.run_until_parked();
    assert_eq!(
        requests.lock().unwrap().as_slice(),
        &[("one".into(), 0), ("one".into(), 7)]
    );
}

#[gpui::test]
fn retention_selection_keeps_control_bounds_and_persists_the_default(cx: &mut TestAppContext) {
    let dir = std::env::temp_dir().join(format!(
        "krill-artifact-retention-ui-{}",
        std::process::id()
    ));
    std::fs::remove_dir_all(&dir).ok();
    krill_proto::artifacts::enable_sharing(&dir).unwrap();
    cx.update(gpui_component::init);
    let (view, vcx) = cx.add_window_view(|_, cx| {
        View::with_calls(
            Arc::new(|_| Ok(page("one", None))),
            Arc::new(|_| Ok(())),
            Some(dir.clone()),
            cx,
        )
    });
    vcx.simulate_resize(gpui::size(px(900.), px(900.)));
    vcx.run_until_parked();
    let before = vcx.debug_bounds("artifacts-default-retention").unwrap();
    view.update(vcx, |v, cx| v.set_default_retention(0, cx));
    vcx.run_until_parked();
    assert_eq!(
        vcx.debug_bounds("artifacts-default-retention").unwrap(),
        before
    );
    assert_eq!(
        krill_proto::artifacts::default_retention_days(&dir).unwrap(),
        0
    );
    view.update(vcx, |v, cx| v.select_slug(Some("one".into()), cx));
    vcx.run_until_parked();
    let drawer = vcx.debug_bounds("artifact-detail-drawer").unwrap();
    let bounds = vcx.debug_bounds("artifact-retention").unwrap();
    assert!(bounds.left() >= drawer.left());
    assert!(bounds.right() <= drawer.right());
    std::fs::remove_dir_all(dir).unwrap();
}

#[gpui::test]
fn escape_clears_query_then_closes_drawer_then_emits_close_and_delete_removes_item(
    cx: &mut TestAppContext,
) {
    let deleted = Arc::new(Mutex::new(Vec::new()));
    let deleted_seen = deleted.clone();
    let call: Call = Arc::new(|_| Ok(page("one", None)));
    let delete_call: DeleteCall = Arc::new(move |slug| {
        deleted_seen.lock().unwrap().push(slug);
        Ok(())
    });
    let closed = Arc::new(Mutex::new(0usize));
    let closed_seen = closed.clone();
    let view = cx.new(|cx| {
        let v = View::with_calls(call, delete_call, None, cx);
        cx.subscribe(&cx.entity(), move |_, _, _: &Close, _| {
            *closed_seen.lock().unwrap() += 1;
        })
        .detach();
        v
    });
    cx.run_until_parked();

    view.update(cx, |v, cx| {
        v.query = "계획".into();
        v.select_slug(Some("one".into()), cx);
    });
    // First Escape clears search query while keeping drawer open.
    view.update(cx, |v, cx| v.handle_escape(None, cx));
    view.read_with(cx, |v, _| {
        assert!(v.query.is_empty());
        assert_eq!(v.selected_slug(), Some("one"));
    });
    assert_eq!(*closed.lock().unwrap(), 0);

    // Second Escape closes detail drawer without closing page.
    view.update(cx, |v, cx| v.handle_escape(None, cx));
    view.read_with(cx, |v, _| {
        assert_eq!(v.selected_slug(), None);
    });
    assert_eq!(*closed.lock().unwrap(), 0);

    // Selecting and deleting removes the item and clears selected_slug.
    view.update(cx, |v, cx| {
        v.select_slug(Some("one".into()), cx);
        v.delete_artifact("one".into(), cx);
    });
    cx.run_until_parked();
    view.read_with(cx, |v, _| {
        assert!(v.items.is_empty());
        assert_eq!(v.selected_slug(), None);
    });
    assert_eq!(deleted.lock().unwrap().as_slice(), &["one"]);

    // Third Escape when drawer is closed emits Close.
    view.update(cx, |v, cx| v.handle_escape(None, cx));
    assert_eq!(*closed.lock().unwrap(), 1);
}

#[gpui::test]
fn narrow_library_keeps_table_actions_reachable_with_horizontal_scroll(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    let (_, visual) =
        cx.add_window_view(|_, cx| View::with_call(Arc::new(|_| Ok(page("one", None))), cx));
    visual.simulate_resize(gpui::size(px(640.), px(480.)));
    visual.run_until_parked();
    let viewport = visual.debug_bounds("artifacts-table-scroll").unwrap();
    let table = visual.debug_bounds("artifacts-table").unwrap();
    assert!(viewport.size.width > px(0.));
    assert!(table.size.width > viewport.size.width);
    let action_before = visual.debug_bounds("artifact-copy-0").unwrap();
    visual.simulate_event(gpui::ScrollWheelEvent {
        position: viewport.center(),
        delta: gpui::ScrollDelta::Pixels(gpui::point(px(-2000.), px(0.))),
        modifiers: gpui::Modifiers::default(),
        touch_phase: gpui::TouchPhase::Moved,
    });
    visual.run_until_parked();
    let action_after = visual.debug_bounds("artifact-copy-0").unwrap();
    assert!(action_after.left() < action_before.left());
    assert!(action_after.left() >= viewport.left());
    assert!(action_after.right() <= viewport.right());
}

#[gpui::test]
fn narrow_artifact_drawer_keeps_actions_inside_page(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    let (view, visual) =
        cx.add_window_view(|_, cx| View::with_call(Arc::new(|_| Ok(page("one", None))), cx));
    visual.simulate_resize(gpui::size(px(360.), px(600.)));
    visual.run_until_parked();
    view.update(visual, |view, cx| view.select_slug(Some("one".into()), cx));
    visual.run_until_parked();
    let drawer = visual.debug_bounds("artifact-detail-drawer").unwrap();
    assert!(drawer.size.width > px(180.));
    assert!(drawer.left() >= px(0.));
    assert!(drawer.right() <= px(360.));
    for selector in [
        "artifact-detail-copy",
        "artifact-detail-open",
        "artifact-detail-delete",
        "artifact-detail-close",
    ] {
        let action = visual.debug_bounds(selector).unwrap();
        assert!(action.left() >= drawer.left());
        assert!(action.right() <= drawer.right());
    }
}

#[gpui::test]
fn detail_drawer_reads_local_source_once_per_selection(cx: &mut TestAppContext) {
    let dir = std::env::temp_dir().join(format!(
        "krill-artifact-local-source-{}",
        std::process::id()
    ));
    std::fs::remove_dir_all(&dir).ok();
    krill_proto::artifacts::enable_sharing(&dir).unwrap();
    let source = dir.join("plan.md");
    let body = format!("line {}\n", "x".repeat(40)).repeat(5_000);
    std::fs::write(&source, body).unwrap();
    let shares = dir.join("artifact-shares.json");
    std::fs::write(
        &shares,
        serde_json::json!({"files": {source.to_string_lossy(): {"slug": "one"}}}).to_string(),
    )
    .unwrap();
    cx.update(gpui_component::init);
    let (view, vcx) = cx.add_window_view(|_, cx| {
        View::with_calls(
            Arc::new(|_| Ok(page("one", None))),
            Arc::new(|_| Ok(())),
            Some(dir.clone()),
            cx,
        )
    });
    vcx.simulate_resize(gpui::size(px(900.), px(900.)));
    vcx.run_until_parked();
    view.update(vcx, |v, cx| v.select_slug(Some("one".into()), cx));
    vcx.run_until_parked();
    assert!(vcx.debug_bounds("artifact-local-source").is_some());

    std::fs::remove_file(&source).unwrap();
    std::fs::remove_file(&shares).unwrap();
    view.update(vcx, |_, cx| cx.notify());
    vcx.run_until_parked();
    assert!(
        vcx.debug_bounds("artifact-local-source").is_some(),
        "a redraw must not read the source again"
    );
    std::fs::remove_dir_all(dir).unwrap();
}

#[gpui::test]
fn artifact_delete_requires_second_click_and_keeps_neighbor_bounds(cx: &mut TestAppContext) {
    let deleted = Arc::new(Mutex::new(Vec::new()));
    let deleted_seen = Arc::clone(&deleted);
    let call: Call = Arc::new(|_| {
        Ok(serde_json::from_value(serde_json::json!({
            "artifacts": [
                {
                    "artifact": {"slug": "one", "originalFileName": "one.html"},
                    "shareUrl": "https://share.krill.page/a/one"
                },
                {
                    "artifact": {"slug": "two", "originalFileName": "two.html"},
                    "shareUrl": "https://share.krill.page/a/two"
                }
            ],
            "nextCursor": null
        }))
        .unwrap())
    });
    let delete_call: DeleteCall = Arc::new(move |slug| {
        deleted_seen.lock().unwrap().push(slug);
        Ok(())
    });
    let closed = Arc::new(Mutex::new(0usize));
    let closed_seen = Arc::clone(&closed);

    cx.update(gpui_component::init);
    let (view, vcx) = cx.add_window_view(|_, cx| {
        let v = View::with_calls(call, delete_call, None, cx);
        cx.subscribe(&cx.entity(), move |_, _, _: &Close, _| {
            *closed_seen.lock().unwrap() += 1;
        })
        .detach();
        v
    });
    vcx.simulate_resize(gpui::size(px(960.), px(640.)));
    vcx.run_until_parked();

    let open_before = vcx.debug_bounds("artifact-open-0").unwrap();
    let del_before = vcx.debug_bounds("artifact-delete-0").unwrap();

    vcx.simulate_click(del_before.center(), gpui::Modifiers::default());
    vcx.run_until_parked();
    assert!(deleted.lock().unwrap().is_empty());
    assert_eq!(vcx.debug_bounds("artifact-open-0"), Some(open_before));
    assert_eq!(vcx.debug_bounds("artifact-delete-0"), Some(del_before));

    // Escape disarms without emitting Close.
    view.update(vcx, |v, cx| v.handle_escape(None, cx));
    vcx.run_until_parked();
    view.read_with(vcx, |v, _| assert!(v.armed_delete.is_none()));
    assert_eq!(*closed.lock().unwrap(), 0);

    // Two clicks on row delete confirm deletion.
    let del_btn = vcx.debug_bounds("artifact-delete-0").unwrap();
    vcx.simulate_click(del_btn.center(), gpui::Modifiers::default());
    vcx.run_until_parked();
    let del_btn = vcx.debug_bounds("artifact-delete-0").unwrap();
    vcx.simulate_click(del_btn.center(), gpui::Modifiers::default());
    vcx.run_until_parked();
    assert_eq!(deleted.lock().unwrap().as_slice(), &["one"]);

    // Detail drawer delete also requires two clicks and keeps sibling bounds even while row is mounted.
    view.update(vcx, |v, cx| v.select_slug(Some("two".into()), cx));
    vcx.run_until_parked();
    let detail_open_before = vcx.debug_bounds("artifact-detail-open").unwrap();
    let detail_del_before = vcx.debug_bounds("artifact-detail-delete").unwrap();

    vcx.simulate_click(detail_del_before.center(), gpui::Modifiers::default());
    vcx.run_until_parked();
    assert_eq!(deleted.lock().unwrap().as_slice(), &["one"]);
    assert_eq!(
        vcx.debug_bounds("artifact-detail-open"),
        Some(detail_open_before)
    );
    assert_eq!(
        vcx.debug_bounds("artifact-detail-delete"),
        Some(detail_del_before)
    );

    let detail_del = vcx.debug_bounds("artifact-detail-delete").unwrap();
    vcx.simulate_click(detail_del.center(), gpui::Modifiers::default());
    vcx.run_until_parked();
    assert_eq!(deleted.lock().unwrap().as_slice(), &["one", "two"]);
    view.read_with(vcx, |v, _| {
        assert!(v.items.is_empty());
        assert_eq!(v.selected_slug(), None);
    });
}

#[gpui::test]
fn artifacts_dynamic_labels_reserve_width_and_icon_buttons_expose_tooltips(
    cx: &mut TestAppContext,
) {
    use std::time::Duration;

    let dir = std::env::temp_dir().join(format!(
        "krill-artifact-reserved-tooltips-{}",
        std::process::id()
    ));
    std::fs::remove_dir_all(&dir).ok();
    krill_proto::artifacts::enable_sharing(&dir).unwrap();

    cx.update(gpui_component::init);
    let (view, vcx) = cx.add_window_view(|_, cx| {
        View::with_calls(
            Arc::new(|_| Ok(page("one", None))),
            Arc::new(|_| Ok(())),
            Some(dir.clone()),
            cx,
        )
    });
    vcx.simulate_resize(gpui::size(px(960.), px(640.)));
    vcx.run_until_parked();

    let sharing_on_bounds = vcx.debug_bounds("artifacts-sharing-toggle").unwrap();
    view.update(vcx, |v, cx| v.toggle_sharing(false, cx));
    vcx.run_until_parked();
    assert_eq!(
        vcx.debug_bounds("artifacts-sharing-toggle"),
        Some(sharing_on_bounds)
    );

    view.update(vcx, |v, cx| {
        v.query = "계획".into();
        cx.notify();
    });
    vcx.run_until_parked();
    let clear_btn = vcx.debug_bounds("artifacts-search-clear").unwrap();
    vcx.simulate_mouse_move(clear_btn.center(), None, gpui::Modifiers::default());
    vcx.executor().advance_clock(Duration::from_millis(600));
    vcx.run_until_parked();
    assert!(vcx.debug_bounds("tooltip:artifacts-search-clear").is_some());

    view.update(vcx, |v, cx| v.clear_search(None, cx));
    vcx.run_until_parked();

    let row_del = vcx.debug_bounds("artifact-delete-0").unwrap();
    vcx.simulate_mouse_move(row_del.center(), None, gpui::Modifiers::default());
    vcx.executor().advance_clock(Duration::from_millis(600));
    vcx.run_until_parked();
    assert!(vcx.debug_bounds("tooltip:artifact-delete-0").is_some());

    view.update(vcx, |v, cx| v.select_slug(Some("one".into()), cx));
    vcx.run_until_parked();
    let copy_before = vcx.debug_bounds("artifact-detail-copy").unwrap();
    vcx.simulate_click(copy_before.center(), gpui::Modifiers::default());
    vcx.run_until_parked();
    assert_eq!(vcx.debug_bounds("artifact-detail-copy"), Some(copy_before));

    let close_btn = vcx.debug_bounds("artifact-detail-close").unwrap();
    vcx.simulate_mouse_move(close_btn.center(), None, gpui::Modifiers::default());
    vcx.executor().advance_clock(Duration::from_millis(600));
    vcx.run_until_parked();
    assert!(vcx.debug_bounds("tooltip:artifact-detail-close").is_some());

    std::fs::remove_dir_all(dir).unwrap();
}
