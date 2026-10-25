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
        "2026-10-25 14:32"
    );
    assert_eq!(format_timestamp_short(Some("2026-11-23")), "2026-11-23");
    assert_eq!(format_timestamp_short(None), "—");
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
