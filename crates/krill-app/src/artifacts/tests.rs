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
